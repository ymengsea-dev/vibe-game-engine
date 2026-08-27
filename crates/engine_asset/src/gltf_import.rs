//! glTF 2.0 import: meshes, materials (data only — no shader support for
//! them yet, that's Milestone 6), perspective cameras, skeletons (skins/
//! joints), and animations (keyframe channels) — sampling an animation
//! against a skeleton lives in `engine_animation`, not here.
//!
//! Built on the `gltf` crate, which already resolves buffers (embedded
//! base64 or GLB binary chunk) and decodes embedded images to raw pixels
//! — this module's job is reshaping that into the engine's own types
//! ([`engine_renderer::Vertex`], [`engine_renderer::Camera`]) and
//! rejecting what it can't represent yet, rather than guessing.

use std::collections::HashMap;

use engine_renderer::{Camera, Projection, Vertex};
use glam::{Mat4, Quat, Vec3};

use crate::error::AssetError;

/// One glTF mesh primitive, converted to this engine's vertex/index
/// format.
///
/// glTF meshes can have multiple primitives (e.g. one per material); each
/// becomes its own [`ImportedMesh`] rather than being merged, since they
/// may use different materials.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedMesh {
    /// The owning glTF mesh's name, if any (not unique per-primitive).
    pub name: Option<String>,
    /// Packed vertex data.
    pub vertices: Vec<Vertex>,
    /// Triangle-list indices into `vertices`.
    pub indices: Vec<u32>,
    /// Per-vertex joint indices/weights, parallel to `vertices`, if the
    /// glTF primitive had both `JOINTS_0` and `WEIGHTS_0` attributes.
    /// `None` for an unskinned mesh.
    pub skin_weights: Option<ImportedSkinWeights>,
    /// Index into [`ImportedGltf::skeletons`] the joint indices in
    /// `skin_weights` refer to, if any node using this mesh also has a
    /// skin. `None` if no such node exists (an unskinned mesh, or one
    /// that carries `JOINTS_0`/`WEIGHTS_0` data but isn't actually used
    /// skinned by any node — technically possible in glTF, imported as
    /// data with no skeleton to resolve against).
    pub skeleton: Option<usize>,
}

/// One mesh primitive's per-vertex skinning data, parallel to that
/// primitive's [`ImportedMesh::vertices`].
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedSkinWeights {
    /// Up to 4 joint indices per vertex — indices into whichever
    /// [`ImportedSkeleton::joints`] the owning [`ImportedMesh::skeleton`]
    /// points at.
    pub joints: Vec<[u16; 4]>,
    /// Each vertex's 4 corresponding weights (glTF requires these sum to
    /// ~1.0 per vertex; not re-validated here — imported as given).
    pub weights: Vec<[f32; 4]>,
}

/// One joint in an imported skeleton hierarchy.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedJoint {
    /// This joint's node's name, if any.
    pub name: Option<String>,
    /// This joint's glTF node index — what an [`ImportedAnimation`]'s
    /// channels are keyed by, so a sampler can look up which channels
    /// (if any) animate this joint.
    pub node_index: usize,
    /// Index into the owning [`ImportedSkeleton::joints`] of this
    /// joint's parent, or `None` if it's a root within this skeleton
    /// (its glTF node may still have a parent node — just not one that's
    /// itself a joint of this same skin).
    pub parent: Option<usize>,
    /// This joint's bind-pose transform, relative to its parent (or to
    /// wherever it sits in the scene, if a root) — the same
    /// translation/rotation/scale decomposition [`engine_utils::Transform`]
    /// always uses, read from the glTF node itself (before any animation
    /// is applied — that's a later iteration).
    pub local_bind_transform: engine_utils::Transform,
    /// Transforms a vertex from mesh-local (bind pose) space into this
    /// joint's local space — glTF's inverse bind matrix. Identity if the
    /// skin omits `inverseBindMatrices` (valid per spec: it means the
    /// bind pose transform *is* the inverse bind matrix).
    pub inverse_bind_matrix: Mat4,
}

/// An imported glTF skin: an ordered list of joints (each an index into
/// this same list away from its parent, forming a hierarchy) plus each
/// joint's inverse bind matrix — everything a future animation system
/// needs to turn a sampled pose into skinning matrices.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ImportedSkeleton {
    /// This skin's name, if any.
    pub name: Option<String>,
    /// This skeleton's joints, in the same order as the glTF skin's
    /// `joints` array — the order [`ImportedSkinWeights::joints`]'
    /// indices refer to.
    pub joints: Vec<ImportedJoint>,
}

/// How to interpolate between two [`ImportedKeyframes`] values.
///
/// glTF's third mode, `CUBICSPLINE` (which also changes the output
/// accessor's layout — three values per keyframe: in-tangent, value,
/// out-tangent — rather than just adding a case here), isn't imported;
/// [`import_gltf_slice`] skips a channel that uses it, the same way it
/// skips an orthographic camera or a morph-target-weight channel
/// elsewhere in this importer — not an error, the rest of the file still
/// imports. Data only for now, matching every other importer type in
/// this module — sampling (turning keyframes plus a point in time into a
/// pose) is a future iteration's job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportedInterpolation {
    /// Interpolate linearly between keyframes (spherically, for
    /// rotations — a sampler's job to know the difference, not this
    /// type's).
    Linear,
    /// Hold each keyframe's value until the next one.
    Step,
}

/// One TRS property's keyframes, imported from one glTF animation
/// sampler. `times`/`values` are always the same non-zero length —
/// [`import_gltf_slice`] rejects anything else.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedKeyframes<T> {
    /// How to interpolate between consecutive `values`.
    pub interpolation: ImportedInterpolation,
    /// Keyframe times, in seconds, strictly ascending (glTF's own
    /// requirement on sampler input accessors — not re-validated here).
    pub times: Vec<f32>,
    /// Each keyframe's value, parallel to `times`.
    pub values: Vec<T>,
}

/// One glTF node's animated TRS channels — a joint (or any other node)
/// need not have all three; an unanimated property keeps whatever value
/// it has outside of sampling (e.g. a skeleton joint's bind pose).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ImportedAnimationChannels {
    /// Animated translation, if this node has a `Translation`-targeting
    /// channel.
    pub translation: Option<ImportedKeyframes<Vec3>>,
    /// Animated rotation, if this node has a `Rotation`-targeting
    /// channel.
    pub rotation: Option<ImportedKeyframes<Quat>>,
    /// Animated scale, if this node has a `Scale`-targeting channel.
    pub scale: Option<ImportedKeyframes<Vec3>>,
}

/// One imported glTF animation ("clip"): every animated node's channels,
/// keyed by glTF node index (matching [`ImportedJoint::node_index`], so
/// a sampler can look up a skeleton joint's channels directly).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ImportedAnimation {
    /// This animation's name, if any.
    pub name: Option<String>,
    /// The latest keyframe time across every channel in this
    /// animation — how long it runs before looping/holding, in seconds.
    /// `0.0` if it has no channels.
    pub duration: f32,
    /// Animated nodes' channels, by glTF node index.
    pub channels: HashMap<usize, ImportedAnimationChannels>,
}

/// A glTF PBR metallic-roughness material's data.
///
/// Data only: nothing in the current renderer pipeline consumes this yet
/// (Milestone 6 adds a real material system) — extracted now so the
/// importer doesn't need revisiting when that lands.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedMaterial {
    /// This material's name, if any.
    pub name: Option<String>,
    /// Base color factor (linear RGBA, `[0, 1]`).
    pub base_color_factor: [f32; 4],
    /// Metallic factor, `[0, 1]`.
    pub metallic_factor: f32,
    /// Roughness factor, `[0, 1]`.
    pub roughness_factor: f32,
    /// Index into [`ImportedGltf::images`] for the base color texture, if
    /// this material has one.
    pub base_color_image: Option<usize>,
}

/// An embedded image, decoded to tightly-packed RGBA8 (ready for
/// [`engine_renderer::GpuContext::create_texture_from_rgba`]).
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedImage {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Tightly-packed RGBA8 pixel data (`width * height * 4` bytes).
    pub rgba8: Vec<u8>,
}

/// A glTF perspective camera, converted to this engine's [`Camera`].
///
/// Orthographic cameras aren't representable by [`Camera`] (perspective
/// only) and are skipped rather than approximated.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedCamera {
    /// This camera's name, if any.
    pub name: Option<String>,
    /// The converted camera. `eye`/`target`/`up` are derived from the
    /// camera node's transform (`target` is `eye + forward`, an arbitrary
    /// distance ahead — glTF stores an orientation, not a look-at point).
    pub camera: Camera,
}

/// Everything [`import_gltf_slice`] extracted from one glTF/GLB file.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ImportedGltf {
    /// Every primitive of every mesh in the file.
    pub meshes: Vec<ImportedMesh>,
    /// Every material in the file.
    pub materials: Vec<ImportedMaterial>,
    /// Every embedded image in the file, decoded to RGBA8.
    pub images: Vec<ImportedImage>,
    /// Every perspective camera node in the file.
    pub cameras: Vec<ImportedCamera>,
    /// Every distinct skin in the file, imported once each even if
    /// shared by multiple nodes — see [`ImportedMesh::skeleton`] for how
    /// a mesh points into this list.
    pub skeletons: Vec<ImportedSkeleton>,
    /// Every animation ("clip") in the file.
    pub animations: Vec<ImportedAnimation>,
}

/// Imports a glTF (`.gltf`, JSON with embedded base64 buffers/images) or
/// GLB (`.glb`, binary) file already in memory.
///
/// # Errors
///
/// Returns [`AssetError::GltfImport`] if `bytes` isn't valid glTF/GLB, if
/// a mesh primitive is missing positions or has mismatched attribute
/// counts, or if an embedded image uses a pixel format this importer
/// doesn't convert (currently: 8-bit RGB and RGBA only).
pub fn import_gltf_slice(bytes: &[u8]) -> Result<ImportedGltf, AssetError> {
    let (document, buffers, images) =
        gltf::import_slice(bytes).map_err(|err| AssetError::GltfImport(err.to_string()))?;

    let out_images = images
        .iter()
        .map(convert_image)
        .collect::<Result<Vec<_>, _>>()?;

    let mut meshes = Vec::new();
    // Parallel to `meshes`: which `document.meshes()` index produced
    // each flat entry — glTF skins attach to *nodes*, not meshes, so the
    // node pass below needs this to know which flat entries a given
    // node's mesh corresponds to.
    let mut owning_mesh_index = Vec::new();
    for mesh in document.meshes() {
        for primitive in mesh.primitives() {
            let reader = primitive
                .reader(|buffer| buffers.get(buffer.index()).map(|data| data.0.as_slice()));

            let positions: Vec<[f32; 3]> = reader
                .read_positions()
                .ok_or_else(|| {
                    AssetError::GltfImport(format!(
                        "mesh {:?} primitive has no POSITION attribute",
                        mesh.name()
                    ))
                })?
                .collect();

            let normals: Vec<[f32; 3]> = match reader.read_normals() {
                Some(iter) => iter.collect(),
                // Flat +Z placeholder for meshes that omit normals (valid
                // glTF; shading will just look wrong until a real normal
                // is supplied — not this importer's job to compute one).
                None => vec![[0.0, 0.0, 1.0]; positions.len()],
            };

            let uvs: Vec<[f32; 2]> = match reader.read_tex_coords(0) {
                Some(iter) => iter.into_f32().collect(),
                None => vec![[0.0, 0.0]; positions.len()],
            };

            if normals.len() != positions.len() || uvs.len() != positions.len() {
                return Err(AssetError::GltfImport(format!(
                    "mesh {:?} primitive has mismatched attribute counts (positions: {}, normals: {}, uvs: {})",
                    mesh.name(),
                    positions.len(),
                    normals.len(),
                    uvs.len()
                )));
            }

            let indices: Vec<u32> = match reader.read_indices() {
                Some(read) => read.into_u32().collect(),
                None => (0..positions.len() as u32).collect(),
            };

            let joints_0: Option<Vec<[u16; 4]>> =
                reader.read_joints(0).map(|iter| iter.into_u16().collect());
            let weights_0: Option<Vec<[f32; 4]>> =
                reader.read_weights(0).map(|iter| iter.into_f32().collect());
            let skin_weights = match (joints_0, weights_0) {
                (Some(joints), Some(weights)) => {
                    if joints.len() != positions.len() || weights.len() != positions.len() {
                        return Err(AssetError::GltfImport(format!(
                            "mesh {:?} primitive has mismatched skin attribute counts (positions: {}, joints: {}, weights: {})",
                            mesh.name(),
                            positions.len(),
                            joints.len(),
                            weights.len()
                        )));
                    }
                    Some(ImportedSkinWeights { joints, weights })
                }
                (None, None) => None,
                _ => {
                    return Err(AssetError::GltfImport(format!(
                        "mesh {:?} primitive has JOINTS_0 without WEIGHTS_0 (or vice versa) — both or neither are required",
                        mesh.name()
                    )));
                }
            };

            let vertices = positions
                .into_iter()
                .zip(normals)
                .zip(uvs)
                .map(|((position, normal), uv)| Vertex {
                    position,
                    normal,
                    uv,
                })
                .collect();

            meshes.push(ImportedMesh {
                name: mesh.name().map(String::from),
                vertices,
                indices,
                skin_weights,
                skeleton: None,
            });
            owning_mesh_index.push(mesh.index());
        }
    }

    let materials = document
        .materials()
        .map(|material| {
            let pbr = material.pbr_metallic_roughness();
            ImportedMaterial {
                name: material.name().map(String::from),
                base_color_factor: pbr.base_color_factor(),
                metallic_factor: pbr.metallic_factor(),
                roughness_factor: pbr.roughness_factor(),
                base_color_image: pbr
                    .base_color_texture()
                    .map(|info| info.texture().source().index()),
            }
        })
        .collect();

    let mut cameras = Vec::new();
    for node in document.nodes() {
        let Some(camera) = node.camera() else {
            continue;
        };
        let gltf::camera::Projection::Perspective(perspective) = camera.projection() else {
            // Orthographic: not representable by our perspective-only
            // Camera type. Skipped, not an error — the rest of the file
            // still imports.
            continue;
        };

        let (translation, rotation, _scale) = node.transform().decomposed();
        let local = engine_utils::Transform {
            translation: Vec3::from_array(translation),
            rotation: Quat::from_array(rotation),
            scale: Vec3::ONE,
        };

        cameras.push(ImportedCamera {
            name: camera.name().map(String::from),
            camera: Camera {
                eye: local.translation,
                target: local.translation + local.forward(),
                up: local.up(),
                aspect_ratio: perspective.aspect_ratio().unwrap_or(16.0 / 9.0),
                projection: Projection::Perspective {
                    fov_y_radians: perspective.yfov(),
                },
                near: perspective.znear(),
                far: perspective.zfar().unwrap_or(1000.0),
            },
        });
    }

    // Every node's parent, by node index — glTF only stores the reverse
    // (each node lists its own children), so this has to be built by
    // scanning all of them once, up front, rather than asked for
    // directly. Shared by every skin's joint-hierarchy resolution below.
    let mut parent_by_node_index: HashMap<usize, usize> = HashMap::new();
    for node in document.nodes() {
        for child in node.children() {
            parent_by_node_index.insert(child.index(), node.index());
        }
    }

    let mut skeletons: Vec<ImportedSkeleton> = Vec::new();
    let mut skeleton_index_by_skin_index: HashMap<usize, usize> = HashMap::new();
    let mut skeleton_index_by_mesh_index: HashMap<usize, usize> = HashMap::new();

    for node in document.nodes() {
        let (Some(mesh_ref), Some(skin)) = (node.mesh(), node.skin()) else {
            continue;
        };

        // Skins can be (and often are) shared by multiple nodes — import
        // each distinct one only once.
        let skeleton_index = *skeleton_index_by_skin_index
            .entry(skin.index())
            .or_insert_with(|| {
                let joint_nodes: Vec<_> = skin.joints().collect();

                let inverse_bind_matrices: Vec<Mat4> = skin
                    .reader(|buffer| buffers.get(buffer.index()).map(|data| data.0.as_slice()))
                    .read_inverse_bind_matrices()
                    .map(|iter| iter.map(|m| Mat4::from_cols_array_2d(&m)).collect())
                    .unwrap_or_else(|| vec![Mat4::IDENTITY; joint_nodes.len()]);

                let joints = joint_nodes
                    .iter()
                    .enumerate()
                    .map(|(i, joint_node)| {
                        // A parent only counts if it's *also* one of this
                        // skin's joints — a joint's actual glTF parent
                        // (e.g. the skeleton root's own parent) commonly
                        // isn't, and that just makes it a root here.
                        let parent = parent_by_node_index.get(&joint_node.index()).and_then(
                            |parent_index| {
                                joint_nodes.iter().position(|n| n.index() == *parent_index)
                            },
                        );
                        let (translation, rotation, scale) = joint_node.transform().decomposed();

                        ImportedJoint {
                            name: joint_node.name().map(String::from),
                            node_index: joint_node.index(),
                            parent,
                            local_bind_transform: engine_utils::Transform {
                                translation: Vec3::from_array(translation),
                                rotation: Quat::from_array(rotation),
                                scale: Vec3::from_array(scale),
                            },
                            inverse_bind_matrix: inverse_bind_matrices
                                .get(i)
                                .copied()
                                .unwrap_or(Mat4::IDENTITY),
                        }
                    })
                    .collect();

                skeletons.push(ImportedSkeleton {
                    name: skin.name().map(String::from),
                    joints,
                });
                skeletons.len() - 1
            });

        skeleton_index_by_mesh_index.insert(mesh_ref.index(), skeleton_index);
    }

    for (mesh, owner_index) in meshes.iter_mut().zip(&owning_mesh_index) {
        mesh.skeleton = skeleton_index_by_mesh_index.get(owner_index).copied();
    }

    let mut animations = Vec::new();
    for animation in document.animations() {
        let mut channels: HashMap<usize, ImportedAnimationChannels> = HashMap::new();
        let mut duration = 0.0f32;

        for channel in animation.channels() {
            let sampler = channel.sampler();
            let interpolation = match sampler.interpolation() {
                gltf::animation::Interpolation::Linear => ImportedInterpolation::Linear,
                gltf::animation::Interpolation::Step => ImportedInterpolation::Step,
                // See `ImportedInterpolation`'s docs: skipped, not an
                // error — its output accessor is laid out completely
                // differently (tangents included), so there's nothing
                // sensible to read from it as a plain keyframe list.
                gltf::animation::Interpolation::CubicSpline => continue,
            };

            let reader =
                channel.reader(|buffer| buffers.get(buffer.index()).map(|data| data.0.as_slice()));

            let Some(times) = reader.read_inputs().map(|iter| iter.collect::<Vec<f32>>()) else {
                continue;
            };
            if times.is_empty() {
                continue;
            }
            if let Some(&last) = times.last() {
                duration = duration.max(last);
            }

            let Some(outputs) = reader.read_outputs() else {
                continue;
            };
            let node_index = channel.target().node().index();
            let entry = channels.entry(node_index).or_default();

            match outputs {
                gltf::animation::util::ReadOutputs::Translations(iter) => {
                    let values: Vec<Vec3> = iter.map(Vec3::from_array).collect();
                    if values.len() != times.len() {
                        return Err(AssetError::GltfImport(format!(
                            "animation {:?} translation channel has {} values for {} keyframe times",
                            animation.name(),
                            values.len(),
                            times.len()
                        )));
                    }
                    entry.translation = Some(ImportedKeyframes {
                        interpolation,
                        times,
                        values,
                    });
                }
                gltf::animation::util::ReadOutputs::Rotations(rotations) => {
                    let values: Vec<Quat> = rotations.into_f32().map(Quat::from_array).collect();
                    if values.len() != times.len() {
                        return Err(AssetError::GltfImport(format!(
                            "animation {:?} rotation channel has {} values for {} keyframe times",
                            animation.name(),
                            values.len(),
                            times.len()
                        )));
                    }
                    entry.rotation = Some(ImportedKeyframes {
                        interpolation,
                        times,
                        values,
                    });
                }
                gltf::animation::util::ReadOutputs::Scales(iter) => {
                    let values: Vec<Vec3> = iter.map(Vec3::from_array).collect();
                    if values.len() != times.len() {
                        return Err(AssetError::GltfImport(format!(
                            "animation {:?} scale channel has {} values for {} keyframe times",
                            animation.name(),
                            values.len(),
                            times.len()
                        )));
                    }
                    entry.scale = Some(ImportedKeyframes {
                        interpolation,
                        times,
                        values,
                    });
                }
                // No morph target support at all yet — this channel just
                // doesn't affect anything this importer produces.
                gltf::animation::util::ReadOutputs::MorphTargetWeights(_) => {}
            }
        }

        animations.push(ImportedAnimation {
            name: animation.name().map(String::from),
            duration,
            channels,
        });
    }

    Ok(ImportedGltf {
        meshes,
        materials,
        images: out_images,
        cameras,
        skeletons,
        animations,
    })
}

fn convert_image(image: &gltf::image::Data) -> Result<ImportedImage, AssetError> {
    let rgba8 = match image.format {
        gltf::image::Format::R8G8B8A8 => image.pixels.clone(),
        gltf::image::Format::R8G8B8 => image
            .pixels
            .chunks_exact(3)
            .flat_map(|rgb| [rgb[0], rgb[1], rgb[2], 255])
            .collect(),
        other => {
            return Err(AssetError::GltfImport(format!(
                "unsupported embedded image format: {other:?} (only 8-bit RGB/RGBA are converted)"
            )));
        }
    };
    Ok(ImportedImage {
        width: image.width,
        height: image.height,
        rgba8,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Packs a GLB (binary glTF) file from a JSON chunk and a binary
    /// chunk, per the GLB container spec: 12-byte header, then each
    /// chunk as `(length: u32 LE, type: [u8;4], data padded to a
    /// multiple of 4)`.
    fn build_glb(json: &str, bin: &[u8]) -> Vec<u8> {
        fn pad4(mut data: Vec<u8>, pad_byte: u8) -> Vec<u8> {
            while !data.len().is_multiple_of(4) {
                data.push(pad_byte);
            }
            data
        }

        let json_chunk = pad4(json.as_bytes().to_vec(), b' ');
        let bin_chunk = pad4(bin.to_vec(), 0);

        let total_len = 12 + 8 + json_chunk.len() + 8 + bin_chunk.len();

        let mut out = Vec::new();
        out.extend_from_slice(b"glTF");
        out.extend_from_slice(&2u32.to_le_bytes());
        out.extend_from_slice(&(total_len as u32).to_le_bytes());

        out.extend_from_slice(&(json_chunk.len() as u32).to_le_bytes());
        out.extend_from_slice(b"JSON");
        out.extend_from_slice(&json_chunk);

        out.extend_from_slice(&(bin_chunk.len() as u32).to_le_bytes());
        out.extend_from_slice(b"BIN\0");
        out.extend_from_slice(&bin_chunk);

        out
    }

    /// Packs one triangle's positions/normals/uvs/u16-indices as raw
    /// little-endian bytes, in the layout the fixture JSON's
    /// `bufferViews` below expect (positions, then normals, then uvs,
    /// then indices — 36 + 36 + 24 + 6 = 102 bytes).
    fn build_triangle_bin() -> Vec<u8> {
        let mut bin = Vec::new();
        for p in [[0.0f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
            for c in p {
                bin.extend_from_slice(&c.to_le_bytes());
            }
        }
        for n in [[0.0f32, 0.0, 1.0]; 3] {
            for c in n {
                bin.extend_from_slice(&c.to_le_bytes());
            }
        }
        for uv in [[0.0f32, 0.0], [1.0, 0.0], [0.0, 1.0]] {
            for c in uv {
                bin.extend_from_slice(&c.to_le_bytes());
            }
        }
        for i in [0u16, 1, 2] {
            bin.extend_from_slice(&i.to_le_bytes());
        }
        bin
    }

    const FIXTURE_JSON: &str = r#"{
        "asset": {"version": "2.0"},
        "scene": 0,
        "scenes": [{"nodes": [0, 1]}],
        "nodes": [
            {"mesh": 0, "name": "Triangle"},
            {"camera": 0, "name": "MainCamera", "translation": [1.0, 2.0, 3.0]}
        ],
        "meshes": [
            {"name": "TriangleMesh", "primitives": [{
                "attributes": {"POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2},
                "indices": 3,
                "material": 0
            }]}
        ],
        "materials": [
            {"name": "Red", "pbrMetallicRoughness": {
                "baseColorFactor": [1.0, 0.0, 0.0, 1.0],
                "metallicFactor": 0.2,
                "roughnessFactor": 0.8
            }}
        ],
        "cameras": [
            {"type": "perspective", "name": "Cam", "perspective": {
                "yfov": 0.8, "znear": 0.1, "zfar": 100.0, "aspectRatio": 1.777
            }}
        ],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3", "min": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 0.0]},
            {"bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC3"},
            {"bufferView": 2, "componentType": 5126, "count": 3, "type": "VEC2"},
            {"bufferView": 3, "componentType": 5123, "count": 3, "type": "SCALAR"}
        ],
        "bufferViews": [
            {"buffer": 0, "byteOffset": 0, "byteLength": 36},
            {"buffer": 0, "byteOffset": 36, "byteLength": 36},
            {"buffer": 0, "byteOffset": 72, "byteLength": 24},
            {"buffer": 0, "byteOffset": 96, "byteLength": 6}
        ],
        "buffers": [{"byteLength": 102}]
    }"#;

    fn fixture_glb() -> Vec<u8> {
        build_glb(FIXTURE_JSON, &build_triangle_bin())
    }

    #[test]
    fn imports_one_triangle_mesh() {
        let imported = import_gltf_slice(&fixture_glb()).unwrap();
        assert_eq!(imported.meshes.len(), 1);

        let mesh = &imported.meshes[0];
        assert_eq!(mesh.name.as_deref(), Some("TriangleMesh"));
        assert_eq!(mesh.vertices.len(), 3);
        assert_eq!(mesh.indices, vec![0, 1, 2]);
        assert_eq!(mesh.vertices[1].position, [1.0, 0.0, 0.0]);
        assert_eq!(mesh.vertices[0].normal, [0.0, 0.0, 1.0]);
        assert_eq!(mesh.vertices[2].uv, [0.0, 1.0]);
    }

    #[test]
    fn imports_material_factors() {
        let imported = import_gltf_slice(&fixture_glb()).unwrap();
        assert_eq!(imported.materials.len(), 1);

        let material = &imported.materials[0];
        assert_eq!(material.name.as_deref(), Some("Red"));
        assert_eq!(material.base_color_factor, [1.0, 0.0, 0.0, 1.0]);
        assert!((material.metallic_factor - 0.2).abs() < 1e-6);
        assert!((material.roughness_factor - 0.8).abs() < 1e-6);
        assert_eq!(material.base_color_image, None);
    }

    #[test]
    fn imports_perspective_camera_from_node_transform() {
        let imported = import_gltf_slice(&fixture_glb()).unwrap();
        assert_eq!(imported.cameras.len(), 1);

        let imported_camera = &imported.cameras[0];
        assert_eq!(imported_camera.name.as_deref(), Some("Cam"));

        let camera = &imported_camera.camera;
        assert_eq!(camera.eye, Vec3::new(1.0, 2.0, 3.0));
        // Identity rotation: forward is -Z, so target sits one unit
        // behind the eye along Z.
        assert_eq!(camera.target, Vec3::new(1.0, 2.0, 2.0));
        assert_eq!(camera.up, Vec3::Y);
        let Projection::Perspective { fov_y_radians } = camera.projection else {
            panic!("expected imported camera to be perspective");
        };
        assert!((fov_y_radians - 0.8).abs() < 1e-6);
        assert!((camera.near - 0.1).abs() < 1e-6);
        assert!((camera.far - 100.0).abs() < 1e-6);
        assert!((camera.aspect_ratio - 1.777).abs() < 1e-6);
    }

    #[test]
    fn no_images_when_none_embedded() {
        let imported = import_gltf_slice(&fixture_glb()).unwrap();
        assert!(imported.images.is_empty());
    }

    #[test]
    fn unskinned_mesh_has_no_skin_weights_or_skeleton() {
        let imported = import_gltf_slice(&fixture_glb()).unwrap();
        assert!(imported.skeletons.is_empty());
        assert_eq!(imported.meshes[0].skin_weights, None);
        assert_eq!(imported.meshes[0].skeleton, None);
    }

    /// Same triangle as [`build_triangle_bin`], with `JOINTS_0`/
    /// `WEIGHTS_0` appended (24 + 48 bytes), then two joints' inverse
    /// bind matrices (128 bytes) — 102 + 24 + 48 + 128 = 302 bytes.
    fn build_skinned_triangle_bin() -> Vec<u8> {
        let mut bin = build_triangle_bin();

        // JOINTS_0: vertex 0 split between joint 0 and 1, vertices 1/2
        // fully on joint 0 and 1 respectively.
        for joints in [[0u16, 1, 0, 0], [0, 0, 0, 0], [1, 0, 0, 0]] {
            for j in joints {
                bin.extend_from_slice(&j.to_le_bytes());
            }
        }

        // WEIGHTS_0, matching the joint assignments above.
        for weights in [
            [0.6f32, 0.4, 0.0, 0.0],
            [1.0, 0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0, 0.0],
        ] {
            for w in weights {
                bin.extend_from_slice(&w.to_le_bytes());
            }
        }

        // Inverse bind matrices: joint 0 (skeleton root) identity, joint
        // 1 (child, bound at local translation (0, 1, 0)) the inverse of
        // that translation — both column-major, as glTF's MAT4 accessors
        // always are.
        let identity: [[f32; 4]; 4] = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let inverse_child_bind: [[f32; 4]; 4] = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, -1.0, 0.0, 1.0],
        ];
        for matrix in [identity, inverse_child_bind] {
            for column in matrix {
                for c in column {
                    bin.extend_from_slice(&c.to_le_bytes());
                }
            }
        }

        bin
    }

    const SKINNED_FIXTURE_JSON: &str = r#"{
        "asset": {"version": "2.0"},
        "scene": 0,
        "scenes": [{"nodes": [0, 1]}],
        "nodes": [
            {"mesh": 0, "skin": 0, "name": "SkinnedTriangle"},
            {"name": "Root", "children": [2]},
            {"name": "Child", "translation": [0.0, 1.0, 0.0]}
        ],
        "skins": [
            {"joints": [1, 2], "inverseBindMatrices": 6, "name": "TriangleSkin"}
        ],
        "meshes": [
            {"name": "SkinnedMesh", "primitives": [{
                "attributes": {
                    "POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2,
                    "JOINTS_0": 4, "WEIGHTS_0": 5
                },
                "indices": 3
            }]}
        ],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3", "min": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 0.0]},
            {"bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC3"},
            {"bufferView": 2, "componentType": 5126, "count": 3, "type": "VEC2"},
            {"bufferView": 3, "componentType": 5123, "count": 3, "type": "SCALAR"},
            {"bufferView": 4, "componentType": 5123, "count": 3, "type": "VEC4"},
            {"bufferView": 5, "componentType": 5126, "count": 3, "type": "VEC4"},
            {"bufferView": 6, "componentType": 5126, "count": 2, "type": "MAT4"}
        ],
        "bufferViews": [
            {"buffer": 0, "byteOffset": 0, "byteLength": 36},
            {"buffer": 0, "byteOffset": 36, "byteLength": 36},
            {"buffer": 0, "byteOffset": 72, "byteLength": 24},
            {"buffer": 0, "byteOffset": 96, "byteLength": 6},
            {"buffer": 0, "byteOffset": 102, "byteLength": 24},
            {"buffer": 0, "byteOffset": 126, "byteLength": 48},
            {"buffer": 0, "byteOffset": 174, "byteLength": 128}
        ],
        "buffers": [{"byteLength": 302}]
    }"#;

    fn skinned_fixture_glb() -> Vec<u8> {
        build_glb(SKINNED_FIXTURE_JSON, &build_skinned_triangle_bin())
    }

    #[test]
    fn imports_skin_weights_parallel_to_vertices() {
        let imported = import_gltf_slice(&skinned_fixture_glb()).unwrap();
        let mesh = &imported.meshes[0];

        let skin_weights = mesh.skin_weights.as_ref().unwrap();
        assert_eq!(
            skin_weights.joints,
            vec![[0, 1, 0, 0], [0, 0, 0, 0], [1, 0, 0, 0]]
        );
        assert_eq!(
            skin_weights.weights,
            vec![
                [0.6, 0.4, 0.0, 0.0],
                [1.0, 0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0, 0.0]
            ]
        );
    }

    #[test]
    fn imports_one_skeleton_with_a_parent_child_joint_pair() {
        let imported = import_gltf_slice(&skinned_fixture_glb()).unwrap();
        assert_eq!(imported.skeletons.len(), 1);

        let skeleton = &imported.skeletons[0];
        assert_eq!(skeleton.name.as_deref(), Some("TriangleSkin"));
        assert_eq!(skeleton.joints.len(), 2);

        let root = &skeleton.joints[0];
        assert_eq!(root.name.as_deref(), Some("Root"));
        assert_eq!(root.parent, None);
        assert_eq!(root.inverse_bind_matrix, Mat4::IDENTITY);

        let child = &skeleton.joints[1];
        assert_eq!(child.name.as_deref(), Some("Child"));
        assert_eq!(child.parent, Some(0));
        assert_eq!(
            child.local_bind_transform.translation,
            Vec3::new(0.0, 1.0, 0.0)
        );
        assert_eq!(
            child.inverse_bind_matrix,
            Mat4::from_translation(Vec3::new(0.0, -1.0, 0.0))
        );
    }

    #[test]
    fn skinned_mesh_points_at_its_skeleton() {
        let imported = import_gltf_slice(&skinned_fixture_glb()).unwrap();
        assert_eq!(imported.meshes[0].skeleton, Some(0));
    }

    /// [`SKINNED_FIXTURE_JSON`] plus a second node (index 3) reusing the
    /// *same* skin (0) — for asserting that a shared skin is imported
    /// once, not once per node that references it.
    const SHARED_SKIN_FIXTURE_JSON: &str = r#"{
        "asset": {"version": "2.0"},
        "scene": 0,
        "scenes": [{"nodes": [0, 1, 3]}],
        "nodes": [
            {"mesh": 0, "skin": 0, "name": "SkinnedTriangle"},
            {"name": "Root", "children": [2]},
            {"name": "Child", "translation": [0.0, 1.0, 0.0]},
            {"mesh": 0, "skin": 0, "name": "SkinnedTriangle2"}
        ],
        "skins": [
            {"joints": [1, 2], "inverseBindMatrices": 6, "name": "TriangleSkin"}
        ],
        "meshes": [
            {"name": "SkinnedMesh", "primitives": [{
                "attributes": {
                    "POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2,
                    "JOINTS_0": 4, "WEIGHTS_0": 5
                },
                "indices": 3
            }]}
        ],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3", "min": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 0.0]},
            {"bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC3"},
            {"bufferView": 2, "componentType": 5126, "count": 3, "type": "VEC2"},
            {"bufferView": 3, "componentType": 5123, "count": 3, "type": "SCALAR"},
            {"bufferView": 4, "componentType": 5123, "count": 3, "type": "VEC4"},
            {"bufferView": 5, "componentType": 5126, "count": 3, "type": "VEC4"},
            {"bufferView": 6, "componentType": 5126, "count": 2, "type": "MAT4"}
        ],
        "bufferViews": [
            {"buffer": 0, "byteOffset": 0, "byteLength": 36},
            {"buffer": 0, "byteOffset": 36, "byteLength": 36},
            {"buffer": 0, "byteOffset": 72, "byteLength": 24},
            {"buffer": 0, "byteOffset": 96, "byteLength": 6},
            {"buffer": 0, "byteOffset": 102, "byteLength": 24},
            {"buffer": 0, "byteOffset": 126, "byteLength": 48},
            {"buffer": 0, "byteOffset": 174, "byteLength": 128}
        ],
        "buffers": [{"byteLength": 302}]
    }"#;

    #[test]
    fn shared_skin_is_imported_only_once() {
        // Two *nodes* (not two mesh definitions — glTF meshes are
        // definitions, referenced by nodes, so this is still one
        // `ImportedMesh`) reference the same skin index.
        let imported = import_gltf_slice(&build_glb(
            SHARED_SKIN_FIXTURE_JSON,
            &build_skinned_triangle_bin(),
        ))
        .unwrap();

        assert_eq!(imported.skeletons.len(), 1);
        assert_eq!(imported.meshes.len(), 1);
        assert_eq!(imported.meshes[0].skeleton, Some(0));
    }

    #[test]
    fn missing_inverse_bind_matrices_default_to_identity() {
        // Valid per spec: omitting `inverseBindMatrices` means every
        // joint's inverse bind matrix is the identity.
        let json = SKINNED_FIXTURE_JSON.replace("\"inverseBindMatrices\": 6, ", "");
        let imported = import_gltf_slice(&build_glb(&json, &build_skinned_triangle_bin())).unwrap();

        let skeleton = &imported.skeletons[0];
        assert_eq!(skeleton.joints[0].inverse_bind_matrix, Mat4::IDENTITY);
        assert_eq!(skeleton.joints[1].inverse_bind_matrix, Mat4::IDENTITY);
    }

    #[test]
    fn rejects_joints_without_matching_weights() {
        let json = SKINNED_FIXTURE_JSON.replace(", \"WEIGHTS_0\": 5", "");
        let err = import_gltf_slice(&build_glb(&json, &build_skinned_triangle_bin())).unwrap_err();
        assert!(matches!(err, AssetError::GltfImport(_)));
    }

    #[test]
    fn rejects_garbage_bytes() {
        let err = import_gltf_slice(&[0u8, 1, 2, 3]).unwrap_err();
        assert!(matches!(err, AssetError::GltfImport(_)));
    }

    #[test]
    fn rejects_empty_input() {
        let err = import_gltf_slice(&[]).unwrap_err();
        assert!(matches!(err, AssetError::GltfImport(_)));
    }

    #[test]
    fn no_animations_when_none_present() {
        let imported = import_gltf_slice(&skinned_fixture_glb()).unwrap();
        assert!(imported.animations.is_empty());
    }

    /// [`build_skinned_triangle_bin`] plus a 2-keyframe LINEAR
    /// translation animation on node 2 ("Child"): times `[0.0, 1.0]` (8
    /// bytes), values `[(0,1,0), (0,2,0)]` (24 bytes) — 302 + 8 + 24 =
    /// 334 bytes.
    fn build_animated_bin() -> Vec<u8> {
        let mut bin = build_skinned_triangle_bin();
        for t in [0.0f32, 1.0] {
            bin.extend_from_slice(&t.to_le_bytes());
        }
        for translation in [[0.0f32, 1.0, 0.0], [0.0, 2.0, 0.0]] {
            for c in translation {
                bin.extend_from_slice(&c.to_le_bytes());
            }
        }
        bin
    }

    const ANIMATED_FIXTURE_JSON: &str = r#"{
        "asset": {"version": "2.0"},
        "scene": 0,
        "scenes": [{"nodes": [0, 1]}],
        "nodes": [
            {"mesh": 0, "skin": 0, "name": "SkinnedTriangle"},
            {"name": "Root", "children": [2]},
            {"name": "Child", "translation": [0.0, 1.0, 0.0]}
        ],
        "skins": [
            {"joints": [1, 2], "inverseBindMatrices": 6, "name": "TriangleSkin"}
        ],
        "meshes": [
            {"name": "SkinnedMesh", "primitives": [{
                "attributes": {
                    "POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2,
                    "JOINTS_0": 4, "WEIGHTS_0": 5
                },
                "indices": 3
            }]}
        ],
        "animations": [
            {
                "name": "Wiggle",
                "channels": [
                    {"sampler": 0, "target": {"node": 2, "path": "translation"}}
                ],
                "samplers": [
                    {"input": 7, "output": 8, "interpolation": "LINEAR"}
                ]
            }
        ],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3", "min": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 0.0]},
            {"bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC3"},
            {"bufferView": 2, "componentType": 5126, "count": 3, "type": "VEC2"},
            {"bufferView": 3, "componentType": 5123, "count": 3, "type": "SCALAR"},
            {"bufferView": 4, "componentType": 5123, "count": 3, "type": "VEC4"},
            {"bufferView": 5, "componentType": 5126, "count": 3, "type": "VEC4"},
            {"bufferView": 6, "componentType": 5126, "count": 2, "type": "MAT4"},
            {"bufferView": 7, "componentType": 5126, "count": 2, "type": "SCALAR", "min": [0.0], "max": [1.0]},
            {"bufferView": 8, "componentType": 5126, "count": 2, "type": "VEC3"}
        ],
        "bufferViews": [
            {"buffer": 0, "byteOffset": 0, "byteLength": 36},
            {"buffer": 0, "byteOffset": 36, "byteLength": 36},
            {"buffer": 0, "byteOffset": 72, "byteLength": 24},
            {"buffer": 0, "byteOffset": 96, "byteLength": 6},
            {"buffer": 0, "byteOffset": 102, "byteLength": 24},
            {"buffer": 0, "byteOffset": 126, "byteLength": 48},
            {"buffer": 0, "byteOffset": 174, "byteLength": 128},
            {"buffer": 0, "byteOffset": 302, "byteLength": 8},
            {"buffer": 0, "byteOffset": 310, "byteLength": 24}
        ],
        "buffers": [{"byteLength": 334}]
    }"#;

    fn animated_fixture_glb() -> Vec<u8> {
        build_glb(ANIMATED_FIXTURE_JSON, &build_animated_bin())
    }

    #[test]
    fn imports_one_animation_with_a_translation_channel() {
        let imported = import_gltf_slice(&animated_fixture_glb()).unwrap();
        assert_eq!(imported.animations.len(), 1);

        let animation = &imported.animations[0];
        assert_eq!(animation.name.as_deref(), Some("Wiggle"));
        assert_eq!(animation.duration, 1.0);

        // Node 2 is "Child" — the skeleton's joint 1.
        let channels = animation.channels.get(&2).unwrap();
        let translation = channels.translation.as_ref().unwrap();
        assert_eq!(translation.interpolation, ImportedInterpolation::Linear);
        assert_eq!(translation.times, vec![0.0, 1.0]);
        assert_eq!(
            translation.values,
            vec![Vec3::new(0.0, 1.0, 0.0), Vec3::new(0.0, 2.0, 0.0)]
        );
        assert!(channels.rotation.is_none());
        assert!(channels.scale.is_none());
    }

    #[test]
    fn animation_channel_keys_match_skeleton_joint_node_indices() {
        let imported = import_gltf_slice(&animated_fixture_glb()).unwrap();
        let skeleton = &imported.skeletons[0];

        // "Child" is joint 1 in the skeleton and node 2 in the document
        // — an animation sampler needs to bridge exactly this gap.
        let child = &skeleton.joints[1];
        assert_eq!(child.node_index, 2);
        assert!(
            imported.animations[0]
                .channels
                .contains_key(&child.node_index)
        );
    }

    #[test]
    fn rejects_mismatched_animation_keyframe_counts() {
        // Accessor 8 (the translation output) claims only 1 value while
        // its sampler's input (accessor 7, the keyframe times) still has
        // 2 — exactly the "output count doesn't match input count" case
        // this engine's own length check exists to catch.
        let json = ANIMATED_FIXTURE_JSON.replacen(
            "{\"bufferView\": 8, \"componentType\": 5126, \"count\": 2, \"type\": \"VEC3\"}",
            "{\"bufferView\": 8, \"componentType\": 5126, \"count\": 1, \"type\": \"VEC3\"}",
            1,
        );
        let err = import_gltf_slice(&build_glb(&json, &build_animated_bin())).unwrap_err();
        assert!(matches!(err, AssetError::GltfImport(_)));
    }
}
