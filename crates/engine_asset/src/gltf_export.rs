//! Writing glTF 2.0 — the inverse of [`crate::import_gltf_slice`].
//!
//! ## Why an engine needs a writer
//!
//! A game whose art is generated rather than modelled still has to put
//! that art somewhere a person can see, browse and replace. Generating
//! geometry into memory at startup leaves an empty `assets/` folder and
//! an asset browser with nothing in it. Writing it to a real `.gltf`
//! file makes the generator an authoring tool — the same role Blender
//! plays — and everything downstream (the asset browser, the importer,
//! the scene format, hot reload) works unchanged.
//!
//! ## Self-contained files only
//!
//! Buffers and images are embedded as base64 `data:` URIs rather than
//! written beside the JSON. Not a style choice:
//! [`crate::import_gltf_slice`] takes bytes with no base path, so it
//! cannot resolve an external `buffer.bin` or `texture.png` reference.
//! One file per model keeps what this crate writes readable by what this
//! crate reads — a property the round-trip test enforces.
//!
//! ## What is written
//!
//! Static meshes, skinned meshes, skins (joint hierarchy plus inverse
//! bind matrices) and animations. Cameras and arbitrary node hierarchies
//! are not: nothing needs them written yet.
//!
//! ## Animation events
//!
//! A clip that says "a foot lands here" is far more useful than one that
//! does not, and glTF has no event track. They go in the animation's
//! `extras`, which is exactly what that field is for, and
//! [`crate::import_gltf_slice`] reads them back. Without this, footstep
//! timing lives in game code while the motion lives in the file, and the
//! two drift the first time somebody retimes the walk.

use std::collections::HashMap;

use engine_renderer::{SkinnedVertex, Vertex};
use engine_utils::Transform;
use glam::{Mat4, Quat, Vec3};
use serde_json::{Map, Value, json};

use crate::error::AssetError;

/// How a material's alpha channel is interpreted, matching glTF's
/// `alphaMode` and [`engine_renderer::AlphaMode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExportAlphaMode {
    /// Alpha ignored; the surface is fully opaque.
    #[default]
    Opaque,
    /// Alpha thresholded against a cutoff — foliage, chain-link, decals.
    Mask,
    /// Alpha blended.
    Blend,
}

impl ExportAlphaMode {
    /// The glTF `alphaMode` string.
    fn as_str(self) -> &'static str {
        match self {
            ExportAlphaMode::Opaque => "OPAQUE",
            ExportAlphaMode::Mask => "MASK",
            ExportAlphaMode::Blend => "BLEND",
        }
    }
}

/// Where a material's texture comes from.
///
/// The choice matters more than it looks. [`ExportTexture::External`]
/// keeps the `.png` in the project's `assets/` folder as the *live*
/// source: edit it in any image editor and the next load of the model
/// picks it up. It costs a constraint — only
/// [`crate::import_gltf_file`] can resolve it, because reading an
/// external reference requires knowing where the model file lives.
///
/// [`ExportTexture::Embedded`] copies the bytes into the model, so a
/// single self-contained file travels anywhere (a packed bundle, a byte
/// stream) at the price of that copy going stale when the source `.png`
/// changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportTexture {
    /// A URI written into the file, relative to the model's own
    /// directory — e.g. `"../textures/grass.png"`.
    External(String),
    /// PNG bytes copied into the model's buffer.
    Embedded(Vec<u8>),
}

/// One material to write. Mirrors the subset of
/// [`crate::ImportedMaterial`] this writer supports.
#[derive(Debug, Clone, Default)]
pub struct ExportMaterial {
    /// Material name, written to the glTF and read back as
    /// [`crate::ImportedMaterial::name`].
    pub name: String,
    /// Linear RGBA base colour factor, multiplied with the texture.
    pub base_color_factor: [f32; 4],
    /// Metallic factor, `[0, 1]`.
    pub metallic_factor: f32,
    /// Roughness factor, `[0, 1]`.
    pub roughness_factor: f32,
    /// This material's base colour texture, if any.
    pub base_color_texture: Option<ExportTexture>,
    /// How to treat alpha.
    pub alpha_mode: ExportAlphaMode,
    /// Threshold for [`ExportAlphaMode::Mask`].
    pub alpha_cutoff: f32,
}

impl ExportMaterial {
    /// An opaque white material named `name`, fully rough and
    /// non-metallic — the sane default for procedural geometry.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            base_color_factor: [1.0, 1.0, 1.0, 1.0],
            metallic_factor: 0.0,
            roughness_factor: 1.0,
            base_color_texture: None,
            alpha_mode: ExportAlphaMode::Opaque,
            alpha_cutoff: 0.5,
        }
    }

    /// This material pointing at a texture file beside the model, given
    /// as a URI relative to the model's directory.
    ///
    /// The file stays the source of truth; see [`ExportTexture`].
    pub fn with_texture_file(mut self, uri: impl Into<String>) -> Self {
        self.base_color_texture = Some(ExportTexture::External(uri.into()));
        self
    }

    /// This material with `png` copied into the model itself.
    pub fn with_embedded_texture(mut self, png: Vec<u8>) -> Self {
        self.base_color_texture = Some(ExportTexture::Embedded(png));
        self
    }

    /// This material as alpha-masked at `cutoff` — for foliage cards.
    pub fn with_alpha_mask(mut self, cutoff: f32) -> Self {
        self.alpha_mode = ExportAlphaMode::Mask;
        self.alpha_cutoff = cutoff;
        self
    }
}

/// One mesh primitive to write.
#[derive(Debug, Clone, Copy)]
pub struct ExportMesh<'a> {
    /// Mesh name, read back as [`crate::ImportedMesh::name`].
    pub name: &'a str,
    /// Vertices, written as `POSITION` / `NORMAL` / `TEXCOORD_0`.
    pub vertices: &'a [Vertex],
    /// Triangle-list indices into `vertices`.
    pub indices: &'a [u32],
    /// Index into the materials slice passed alongside, if any.
    pub material: Option<usize>,
}

/// One joint of an [`ExportSkin`].
#[derive(Debug, Clone)]
pub struct ExportJoint {
    /// Joint name, read back as [`crate::ImportedJoint::name`].
    pub name: String,
    /// Index into the owning skin's joints of this joint's parent, or
    /// `None` for a root.
    pub parent: Option<usize>,
    /// This joint's rest pose, relative to its parent.
    pub local_bind_transform: Transform,
    /// Transforms a vertex from mesh space into this joint's space —
    /// glTF's inverse bind matrix.
    pub inverse_bind_matrix: Mat4,
}

/// A skin: an ordered joint hierarchy that skinned vertices index into.
#[derive(Debug, Clone)]
pub struct ExportSkin {
    /// Skin name.
    pub name: String,
    /// Joints, in the order `JOINTS_0` indices refer to. A joint's
    /// parent must appear before it.
    pub joints: Vec<ExportJoint>,
}

/// One skinned mesh primitive to write.
#[derive(Debug, Clone, Copy)]
pub struct ExportSkinnedMesh<'a> {
    /// Mesh name.
    pub name: &'a str,
    /// Vertices, written as `POSITION`/`NORMAL`/`TEXCOORD_0` plus
    /// `JOINTS_0`/`WEIGHTS_0`.
    pub vertices: &'a [SkinnedVertex],
    /// Triangle-list indices.
    pub indices: &'a [u32],
    /// Index into the materials slice, if any.
    pub material: Option<usize>,
    /// Index into the skins slice this mesh is bound to.
    pub skin: usize,
}

impl ExportSkin {
    /// The export form of an imported skeleton.
    ///
    /// Lets a tool round-trip a skin it loaded, and lets a generator
    /// that already speaks the importer's types (as procedural
    /// characters do) write itself out without a second vocabulary.
    pub fn from_imported(skeleton: &crate::ImportedSkeleton) -> Self {
        Self {
            name: skeleton.name.clone().unwrap_or_else(|| "skin".to_string()),
            joints: skeleton
                .joints
                .iter()
                .map(|joint| ExportJoint {
                    name: joint.name.clone().unwrap_or_else(|| "joint".to_string()),
                    parent: joint.parent,
                    local_bind_transform: joint.local_bind_transform,
                    inverse_bind_matrix: joint.inverse_bind_matrix,
                })
                .collect(),
        }
    }
}

impl ExportAnimation {
    /// The export form of an imported clip, targeting `skin`.
    ///
    /// An imported clip keys its channels by glTF *node* index, so this
    /// needs `skeleton` to map each one back to a joint position. A
    /// channel naming a node that is not one of `skeleton`'s joints is
    /// dropped — it drives something outside the skin, which this writer
    /// has no node for.
    pub fn from_imported(
        animation: &crate::ImportedAnimation,
        skeleton: &crate::ImportedSkeleton,
        skin: usize,
    ) -> Self {
        let joint_of_node: HashMap<usize, usize> = skeleton
            .joints
            .iter()
            .enumerate()
            .map(|(index, joint)| (joint.node_index, index))
            .collect();

        let mut channels = Vec::new();
        for (node, node_channels) in &animation.channels {
            let Some(&joint) = joint_of_node.get(node) else {
                continue;
            };
            if let Some(keys) = &node_channels.translation {
                channels.push(ExportChannel {
                    joint,
                    times: keys.times.clone(),
                    values: ExportChannelValues::Translation(keys.values.clone()),
                });
            }
            if let Some(keys) = &node_channels.rotation {
                channels.push(ExportChannel {
                    joint,
                    times: keys.times.clone(),
                    values: ExportChannelValues::Rotation(keys.values.clone()),
                });
            }
            if let Some(keys) = &node_channels.scale {
                channels.push(ExportChannel {
                    joint,
                    times: keys.times.clone(),
                    values: ExportChannelValues::Scale(keys.values.clone()),
                });
            }
        }
        // Channel order comes out of a hash map otherwise, which would
        // make two bakes of the same clip produce different bytes.
        channels.sort_by_key(|channel| channel.joint);

        Self {
            name: animation.name.clone().unwrap_or_else(|| "clip".to_string()),
            skin,
            channels,
            events: animation
                .events
                .iter()
                .map(|event| ExportAnimationEvent {
                    time: event.time,
                    name: event.name.clone(),
                })
                .collect(),
        }
    }
}

/// The values one animation channel drives.
#[derive(Debug, Clone, PartialEq)]
pub enum ExportChannelValues {
    /// Translation keyframes.
    Translation(Vec<Vec3>),
    /// Rotation keyframes.
    Rotation(Vec<Quat>),
    /// Scale keyframes.
    Scale(Vec<Vec3>),
}

/// One animated joint property.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportChannel {
    /// Index into the skin's joints of the joint this drives.
    pub joint: usize,
    /// Keyframe times in seconds, strictly ascending.
    pub times: Vec<f32>,
    /// Keyframe values, parallel to `times`.
    pub values: ExportChannelValues,
}

/// A named point in a clip, written to the animation's `extras`.
///
/// glTF has no event track; `extras` is its sanctioned place for exactly
/// this. Keeping events beside the motion they belong to is what stops a
/// footstep sound drifting out of sync the first time a clip is retimed.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportAnimationEvent {
    /// When it fires, in seconds from the clip's start.
    pub time: f32,
    /// What fires — game code matches on this.
    pub name: String,
}

/// One animation clip.
#[derive(Debug, Clone)]
pub struct ExportAnimation {
    /// Clip name.
    pub name: String,
    /// Index into the skins slice whose joints the channels target.
    pub skin: usize,
    /// The animated properties.
    pub channels: Vec<ExportChannel>,
    /// Named moments in the clip.
    pub events: Vec<ExportAnimationEvent>,
}

/// Everything one glTF file holds, for [`export_gltf_scene`].
#[derive(Default)]
pub struct GltfScene<'a> {
    /// Unskinned meshes.
    pub meshes: &'a [ExportMesh<'a>],
    /// Skinned meshes, each bound to one of `skins`.
    pub skinned_meshes: &'a [ExportSkinnedMesh<'a>],
    /// Joint hierarchies.
    pub skins: &'a [ExportSkin],
    /// Animation clips.
    pub animations: &'a [ExportAnimation],
    /// Materials both mesh kinds index into.
    pub materials: &'a [ExportMaterial],
}

/// glTF component type for `f32`.
const COMPONENT_FLOAT: u32 = 5126;
/// glTF component type for `u32`.
const COMPONENT_UNSIGNED_INT: u32 = 5125;
/// glTF component type for `u16`.
const COMPONENT_UNSIGNED_SHORT: u32 = 5123;
/// glTF target for a vertex attribute buffer view.
const TARGET_ARRAY_BUFFER: u32 = 34962;
/// glTF target for an index buffer view.
const TARGET_ELEMENT_ARRAY_BUFFER: u32 = 34963;

/// Serializes `meshes` and `materials` as one self-contained glTF 2.0
/// document, returning the `.gltf` file's bytes.
///
/// Every mesh becomes one glTF mesh with a single triangle-list
/// primitive, and every mesh gets a node in the default scene, so an
/// importer that walks nodes (as this crate's does) finds all of them.
///
/// # Errors
///
/// [`AssetError::GltfExport`] if a mesh has no vertices or no indices,
/// if its index count is not a multiple of three, if any index is out of
/// range for its vertex list, if a material index does not exist, or if
/// the assembled JSON cannot be serialized. These are generator bugs,
/// and a file that fails to import later is far more expensive to
/// diagnose than a refusal here.
pub fn export_gltf(
    meshes: &[ExportMesh<'_>],
    materials: &[ExportMaterial],
) -> Result<Vec<u8>, AssetError> {
    export_gltf_scene(&GltfScene {
        meshes,
        materials,
        ..Default::default()
    })
}

/// Serializes a whole [`GltfScene`] — meshes, skinned meshes, skins and
/// animations — as one self-contained glTF 2.0 document.
///
/// Joint nodes come first in the node list, then one node per mesh, so a
/// skin's `joints` array and a mesh node's `skin` reference resolve
/// against stable indices.
///
/// # Errors
///
/// [`AssetError::GltfExport`] for everything [`export_gltf`] reports,
/// plus: a skin with no joints, a joint whose parent appears after it
/// (glTF node hierarchies must be writable in order), a skinned mesh
/// naming a skin that does not exist, a vertex weighting a joint the
/// skin does not have, or an animation channel whose times and values
/// have different lengths.
pub fn export_gltf_scene(scene: &GltfScene<'_>) -> Result<Vec<u8>, AssetError> {
    let GltfScene {
        meshes,
        skinned_meshes,
        skins,
        animations,
        materials,
    } = *scene;

    if meshes.is_empty() && skinned_meshes.is_empty() {
        return Err(AssetError::GltfExport("no meshes to export".to_string()));
    }
    validate(meshes, materials)?;
    validate_skinned(skinned_meshes, skins, materials)?;
    validate_animations(animations, skins)?;

    let mut buffer: Vec<u8> = Vec::new();
    let mut views: Vec<Value> = Vec::new();
    let mut accessors: Vec<Value> = Vec::new();
    let mut gltf_meshes: Vec<Value> = Vec::new();
    let mut nodes: Vec<Value> = Vec::new();

    // Joint nodes first, so their indices are stable and known before
    // anything references them: a skin's `joints` array, a mesh node's
    // `skin`, and every animation channel's target all point here.
    let mut joint_nodes: Vec<Vec<usize>> = Vec::new();
    for skin in skins {
        let base = nodes.len();
        let mut children: Vec<Vec<usize>> = vec![Vec::new(); skin.joints.len()];
        for (index, joint) in skin.joints.iter().enumerate() {
            if let Some(parent) = joint.parent {
                children[parent].push(base + index);
            }
        }
        for (index, joint) in skin.joints.iter().enumerate() {
            let mut node = Map::new();
            node.insert("name".to_string(), json!(joint.name));
            node.insert(
                "translation".to_string(),
                json!(joint.local_bind_transform.translation.to_array()),
            );
            node.insert(
                "rotation".to_string(),
                json!(joint.local_bind_transform.rotation.to_array()),
            );
            node.insert(
                "scale".to_string(),
                json!(joint.local_bind_transform.scale.to_array()),
            );
            if !children[index].is_empty() {
                node.insert("children".to_string(), json!(children[index]));
            }
            nodes.push(Value::Object(node));
        }
        joint_nodes.push((base..nodes.len()).collect());
    }

    for mesh in meshes {
        let positions = accessor_for(
            &mut buffer,
            &mut views,
            &mut accessors,
            mesh.vertices.iter().flat_map(|v| v.position),
            mesh.vertices.len(),
            "VEC3",
            COMPONENT_FLOAT,
            TARGET_ARRAY_BUFFER,
            // Only POSITION carries min/max, and the spec requires it
            // there: an importer uses it for bounds without decoding the
            // buffer.
            Some(bounds(mesh.vertices)),
        );
        let normals = accessor_for(
            &mut buffer,
            &mut views,
            &mut accessors,
            mesh.vertices.iter().flat_map(|v| v.normal),
            mesh.vertices.len(),
            "VEC3",
            COMPONENT_FLOAT,
            TARGET_ARRAY_BUFFER,
            None,
        );
        let uvs = accessor_for(
            &mut buffer,
            &mut views,
            &mut accessors,
            mesh.vertices.iter().flat_map(|v| v.uv),
            mesh.vertices.len(),
            "VEC2",
            COMPONENT_FLOAT,
            TARGET_ARRAY_BUFFER,
            None,
        );
        let indices = accessor_for(
            &mut buffer,
            &mut views,
            &mut accessors,
            mesh.indices.iter().copied(),
            mesh.indices.len(),
            "SCALAR",
            COMPONENT_UNSIGNED_INT,
            TARGET_ELEMENT_ARRAY_BUFFER,
            None,
        );

        let mut primitive = Map::new();
        primitive.insert(
            "attributes".to_string(),
            json!({ "POSITION": positions, "NORMAL": normals, "TEXCOORD_0": uvs }),
        );
        primitive.insert("indices".to_string(), json!(indices));
        // Mode 4 is TRIANGLES, the only topology this writer emits.
        primitive.insert("mode".to_string(), json!(4));
        if let Some(material) = mesh.material {
            primitive.insert("material".to_string(), json!(material));
        }

        nodes.push(json!({ "mesh": gltf_meshes.len(), "name": mesh.name }));
        gltf_meshes.push(json!({
            "name": mesh.name,
            "primitives": [Value::Object(primitive)],
        }));
    }

    for mesh in skinned_meshes {
        let positions = accessor_for(
            &mut buffer,
            &mut views,
            &mut accessors,
            mesh.vertices.iter().flat_map(|v| v.position),
            mesh.vertices.len(),
            "VEC3",
            COMPONENT_FLOAT,
            TARGET_ARRAY_BUFFER,
            Some(skinned_bounds(mesh.vertices)),
        );
        let normals = accessor_for(
            &mut buffer,
            &mut views,
            &mut accessors,
            mesh.vertices.iter().flat_map(|v| v.normal),
            mesh.vertices.len(),
            "VEC3",
            COMPONENT_FLOAT,
            TARGET_ARRAY_BUFFER,
            None,
        );
        let uvs = accessor_for(
            &mut buffer,
            &mut views,
            &mut accessors,
            mesh.vertices.iter().flat_map(|v| v.uv),
            mesh.vertices.len(),
            "VEC2",
            COMPONENT_FLOAT,
            TARGET_ARRAY_BUFFER,
            None,
        );
        // Joint indices as `u16`: glTF's own accessor type for
        // `JOINTS_0`, and no skin here comes close to 65k joints.
        let joints = accessor_for(
            &mut buffer,
            &mut views,
            &mut accessors,
            mesh.vertices
                .iter()
                .flat_map(|v| v.joints.map(|index| index as u16)),
            mesh.vertices.len(),
            "VEC4",
            COMPONENT_UNSIGNED_SHORT,
            TARGET_ARRAY_BUFFER,
            None,
        );
        let weights = accessor_for(
            &mut buffer,
            &mut views,
            &mut accessors,
            mesh.vertices.iter().flat_map(|v| v.weights),
            mesh.vertices.len(),
            "VEC4",
            COMPONENT_FLOAT,
            TARGET_ARRAY_BUFFER,
            None,
        );
        let indices = accessor_for(
            &mut buffer,
            &mut views,
            &mut accessors,
            mesh.indices.iter().copied(),
            mesh.indices.len(),
            "SCALAR",
            COMPONENT_UNSIGNED_INT,
            TARGET_ELEMENT_ARRAY_BUFFER,
            None,
        );

        let mut primitive = Map::new();
        primitive.insert(
            "attributes".to_string(),
            json!({
                "POSITION": positions,
                "NORMAL": normals,
                "TEXCOORD_0": uvs,
                "JOINTS_0": joints,
                "WEIGHTS_0": weights,
            }),
        );
        primitive.insert("indices".to_string(), json!(indices));
        primitive.insert("mode".to_string(), json!(4));
        if let Some(material) = mesh.material {
            primitive.insert("material".to_string(), json!(material));
        }

        nodes.push(json!({
            "mesh": gltf_meshes.len(),
            "skin": mesh.skin,
            "name": mesh.name,
        }));
        gltf_meshes.push(json!({
            "name": mesh.name,
            "primitives": [Value::Object(primitive)],
        }));
    }

    // Skins, once their joint nodes exist and the inverse bind matrices
    // are in the buffer.
    let gltf_skins: Vec<Value> = skins
        .iter()
        .enumerate()
        .map(|(index, skin)| {
            let matrices = accessor_for(
                &mut buffer,
                &mut views,
                &mut accessors,
                skin.joints
                    .iter()
                    .flat_map(|joint| joint.inverse_bind_matrix.to_cols_array()),
                skin.joints.len(),
                "MAT4",
                COMPONENT_FLOAT,
                // No target: this is neither vertex nor index data.
                0,
                None,
            );
            json!({
                "name": skin.name,
                "joints": joint_nodes[index],
                "inverseBindMatrices": matrices,
            })
        })
        .collect();

    let gltf_animations: Vec<Value> = animations
        .iter()
        .map(|animation| {
            let mut samplers = Vec::new();
            let mut channels = Vec::new();
            for channel in &animation.channels {
                let input = accessor_for(
                    &mut buffer,
                    &mut views,
                    &mut accessors,
                    channel.times.iter().copied(),
                    channel.times.len(),
                    "SCALAR",
                    COMPONENT_FLOAT,
                    0,
                    None,
                );
                let (output, path) = match &channel.values {
                    ExportChannelValues::Translation(values) => (
                        accessor_for(
                            &mut buffer,
                            &mut views,
                            &mut accessors,
                            values.iter().flat_map(Vec3::to_array),
                            values.len(),
                            "VEC3",
                            COMPONENT_FLOAT,
                            0,
                            None,
                        ),
                        "translation",
                    ),
                    ExportChannelValues::Rotation(values) => (
                        accessor_for(
                            &mut buffer,
                            &mut views,
                            &mut accessors,
                            values.iter().flat_map(Quat::to_array),
                            values.len(),
                            "VEC4",
                            COMPONENT_FLOAT,
                            0,
                            None,
                        ),
                        "rotation",
                    ),
                    ExportChannelValues::Scale(values) => (
                        accessor_for(
                            &mut buffer,
                            &mut views,
                            &mut accessors,
                            values.iter().flat_map(Vec3::to_array),
                            values.len(),
                            "VEC3",
                            COMPONENT_FLOAT,
                            0,
                            None,
                        ),
                        "scale",
                    ),
                };

                channels.push(json!({
                    "sampler": samplers.len(),
                    "target": {
                        "node": joint_nodes[animation.skin][channel.joint],
                        "path": path,
                    },
                }));
                samplers.push(json!({
                    "input": input,
                    "output": output,
                    "interpolation": "LINEAR",
                }));
            }

            let mut out = Map::new();
            out.insert("name".to_string(), json!(animation.name));
            out.insert("samplers".to_string(), Value::Array(samplers));
            out.insert("channels".to_string(), Value::Array(channels));
            if !animation.events.is_empty() {
                // glTF has no event track; `extras` is its sanctioned
                // place for data a format does not model.
                out.insert(
                    "extras".to_string(),
                    json!({
                        "events": animation
                            .events
                            .iter()
                            .map(|event| json!({ "time": event.time, "name": event.name }))
                            .collect::<Vec<_>>(),
                    }),
                );
            }
            Value::Object(out)
        })
        .collect();

    let (images, textures, samplers) = image_tables(materials, &mut buffer, &mut views);
    let gltf_materials: Vec<Value> = materials
        .iter()
        .scan(0usize, |texture_index, material| {
            let mut pbr = Map::new();
            pbr.insert(
                "baseColorFactor".to_string(),
                json!(material.base_color_factor),
            );
            pbr.insert(
                "metallicFactor".to_string(),
                json!(material.metallic_factor),
            );
            pbr.insert(
                "roughnessFactor".to_string(),
                json!(material.roughness_factor),
            );
            if material.base_color_texture.is_some() {
                pbr.insert(
                    "baseColorTexture".to_string(),
                    json!({ "index": *texture_index }),
                );
                *texture_index += 1;
            }

            let mut out = Map::new();
            out.insert("name".to_string(), json!(material.name));
            out.insert("pbrMetallicRoughness".to_string(), Value::Object(pbr));
            out.insert("alphaMode".to_string(), json!(material.alpha_mode.as_str()));
            if material.alpha_mode == ExportAlphaMode::Mask {
                out.insert("alphaCutoff".to_string(), json!(material.alpha_cutoff));
            }
            Some(Value::Object(out))
        })
        .collect();

    let mut document = Map::new();
    document.insert(
        "asset".to_string(),
        json!({ "version": "2.0", "generator": "RustyEngine engine_asset::export_gltf" }),
    );
    document.insert("scene".to_string(), json!(0));
    document.insert(
        "scenes".to_string(),
        json!([{ "nodes": (0..nodes.len()).collect::<Vec<_>>() }]),
    );
    document.insert("nodes".to_string(), Value::Array(nodes));
    document.insert("meshes".to_string(), Value::Array(gltf_meshes));
    document.insert("accessors".to_string(), Value::Array(accessors));
    document.insert("bufferViews".to_string(), Value::Array(views));
    document.insert(
        "buffers".to_string(),
        json!([{
            "byteLength": buffer.len(),
            "uri": data_uri("application/octet-stream", &buffer),
        }]),
    );
    if !gltf_skins.is_empty() {
        document.insert("skins".to_string(), Value::Array(gltf_skins));
    }
    if !gltf_animations.is_empty() {
        document.insert("animations".to_string(), Value::Array(gltf_animations));
    }
    if !gltf_materials.is_empty() {
        document.insert("materials".to_string(), Value::Array(gltf_materials));
    }
    if !images.is_empty() {
        document.insert("images".to_string(), Value::Array(images));
        document.insert("textures".to_string(), Value::Array(textures));
        document.insert("samplers".to_string(), Value::Array(samplers));
    }

    serde_json::to_vec_pretty(&Value::Object(document))
        .map_err(|err| AssetError::GltfExport(err.to_string()))
}

/// Rejects geometry that would produce a file this crate's importer, or
/// any other, would choke on.
fn validate(meshes: &[ExportMesh<'_>], materials: &[ExportMaterial]) -> Result<(), AssetError> {
    for mesh in meshes {
        if mesh.vertices.is_empty() {
            return Err(AssetError::GltfExport(format!(
                "mesh '{}' has no vertices",
                mesh.name
            )));
        }
        if mesh.indices.is_empty() {
            return Err(AssetError::GltfExport(format!(
                "mesh '{}' has no indices",
                mesh.name
            )));
        }
        if !mesh.indices.len().is_multiple_of(3) {
            return Err(AssetError::GltfExport(format!(
                "mesh '{}' has {} indices, which is not a whole number of triangles",
                mesh.name,
                mesh.indices.len()
            )));
        }
        if let Some(&out_of_range) = mesh
            .indices
            .iter()
            .find(|&&index| index as usize >= mesh.vertices.len())
        {
            return Err(AssetError::GltfExport(format!(
                "mesh '{}' indexes vertex {out_of_range}, past its {} vertices",
                mesh.name,
                mesh.vertices.len()
            )));
        }
        if let Some(material) = mesh.material
            && material >= materials.len()
        {
            return Err(AssetError::GltfExport(format!(
                "mesh '{}' uses material {material}, but only {} were given",
                mesh.name,
                materials.len()
            )));
        }
    }
    Ok(())
}

/// Rejects skinned geometry that would produce an unloadable file.
fn validate_skinned(
    meshes: &[ExportSkinnedMesh<'_>],
    skins: &[ExportSkin],
    materials: &[ExportMaterial],
) -> Result<(), AssetError> {
    for (index, skin) in skins.iter().enumerate() {
        if skin.joints.is_empty() {
            return Err(AssetError::GltfExport(format!(
                "skin '{}' has no joints",
                skin.name
            )));
        }
        for (joint_index, joint) in skin.joints.iter().enumerate() {
            match joint.parent {
                // A parent later in the list cannot be wired up while
                // writing nodes in order, and a self-parent is a cycle.
                Some(parent) if parent >= joint_index => {
                    return Err(AssetError::GltfExport(format!(
                        "skin '{}': joint '{}' names parent {parent}, which is not before it",
                        skin.name, joint.name
                    )));
                }
                _ => {}
            }
        }
        let _ = index;
    }

    for mesh in meshes {
        if mesh.vertices.is_empty() || mesh.indices.is_empty() {
            return Err(AssetError::GltfExport(format!(
                "skinned mesh '{}' has no geometry",
                mesh.name
            )));
        }
        if !mesh.indices.len().is_multiple_of(3) {
            return Err(AssetError::GltfExport(format!(
                "skinned mesh '{}' has {} indices, which is not a whole number of triangles",
                mesh.name,
                mesh.indices.len()
            )));
        }
        if let Some(&out_of_range) = mesh
            .indices
            .iter()
            .find(|&&index| index as usize >= mesh.vertices.len())
        {
            return Err(AssetError::GltfExport(format!(
                "skinned mesh '{}' indexes vertex {out_of_range}, past its {} vertices",
                mesh.name,
                mesh.vertices.len()
            )));
        }
        let Some(skin) = skins.get(mesh.skin) else {
            return Err(AssetError::GltfExport(format!(
                "skinned mesh '{}' uses skin {}, but only {} were given",
                mesh.name,
                mesh.skin,
                skins.len()
            )));
        };
        // A vertex weighted to a joint that does not exist would sample
        // a garbage matrix and fling the mesh across the world.
        for vertex in mesh.vertices {
            for (joint, weight) in vertex.joints.iter().zip(vertex.weights) {
                if weight != 0.0 && *joint as usize >= skin.joints.len() {
                    return Err(AssetError::GltfExport(format!(
                        "skinned mesh '{}' weights joint {joint}, past skin '{}'\'s {} joints",
                        mesh.name,
                        skin.name,
                        skin.joints.len()
                    )));
                }
            }
        }
        if let Some(material) = mesh.material
            && material >= materials.len()
        {
            return Err(AssetError::GltfExport(format!(
                "skinned mesh '{}' uses material {material}, but only {} were given",
                mesh.name,
                materials.len()
            )));
        }
    }
    Ok(())
}

/// Rejects animations that reference joints or skins that do not exist,
/// or whose keyframe times and values disagree.
fn validate_animations(
    animations: &[ExportAnimation],
    skins: &[ExportSkin],
) -> Result<(), AssetError> {
    for animation in animations {
        let Some(skin) = skins.get(animation.skin) else {
            return Err(AssetError::GltfExport(format!(
                "animation '{}' targets skin {}, but only {} were given",
                animation.name,
                animation.skin,
                skins.len()
            )));
        };
        for channel in &animation.channels {
            if channel.joint >= skin.joints.len() {
                return Err(AssetError::GltfExport(format!(
                    "animation '{}' drives joint {}, past skin '{}'\'s {} joints",
                    animation.name,
                    channel.joint,
                    skin.name,
                    skin.joints.len()
                )));
            }
            let values = match &channel.values {
                ExportChannelValues::Translation(values) | ExportChannelValues::Scale(values) => {
                    values.len()
                }
                ExportChannelValues::Rotation(values) => values.len(),
            };
            if values != channel.times.len() {
                return Err(AssetError::GltfExport(format!(
                    "animation '{}': joint {} has {} keyframe times but {values} values",
                    animation.name,
                    channel.joint,
                    channel.times.len()
                )));
            }
            if channel.times.is_empty() {
                return Err(AssetError::GltfExport(format!(
                    "animation '{}': joint {} has no keyframes",
                    animation.name, channel.joint
                )));
            }
        }
    }
    Ok(())
}

/// [`bounds`] for skinned vertices.
fn skinned_bounds(vertices: &[SkinnedVertex]) -> ([f32; 3], [f32; 3]) {
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for vertex in vertices {
        for axis in 0..3 {
            min[axis] = min[axis].min(vertex.position[axis]);
            max[axis] = max[axis].max(vertex.position[axis]);
        }
    }
    (min, max)
}

/// The `images` / `textures` / `samplers` tables for whichever materials
/// carry a texture, in material order, appending each PNG to `buffer`.
///
/// An [`ExportTexture::Embedded`] image goes in a buffer view rather
/// than a `data:` URI of its own: `gltf::import_slice` — what
/// [`crate::import_gltf_slice`] calls — treats *any* URI-referenced
/// image as an external reference and refuses it, even when the URI
/// carries the bytes inline. Buffer views are the form it accepts, and
/// the form GLB uses. An [`ExportTexture::External`] image is a URI by
/// definition, and needs [`crate::import_gltf_file`] to read back.
fn image_tables(
    materials: &[ExportMaterial],
    buffer: &mut Vec<u8>,
    views: &mut Vec<Value>,
) -> (Vec<Value>, Vec<Value>, Vec<Value>) {
    let mut images = Vec::new();
    let mut textures = Vec::new();
    for material in materials {
        let Some(texture) = &material.base_color_texture else {
            continue;
        };
        textures.push(json!({ "source": images.len(), "sampler": 0 }));
        match texture {
            ExportTexture::External(uri) => images.push(json!({
                "name": material.name,
                "uri": uri,
            })),
            ExportTexture::Embedded(png) => {
                while !buffer.len().is_multiple_of(4) {
                    buffer.push(0);
                }
                let offset = buffer.len();
                buffer.extend_from_slice(png);
                let view = views.len();
                // No `target`: a view holding image bytes is not a vertex
                // or index buffer, and naming one confuses strict
                // validators.
                views.push(json!({
                    "buffer": 0,
                    "byteOffset": offset,
                    "byteLength": png.len(),
                }));
                images.push(json!({
                    "name": material.name,
                    "mimeType": "image/png",
                    "bufferView": view,
                }));
            }
        }
    }
    // One shared sampler: linear filtering, repeat wrap — matching what
    // `engine_renderer::Texture` creates for an imported texture.
    let samplers = if images.is_empty() {
        Vec::new()
    } else {
        vec![json!({ "magFilter": 9729, "minFilter": 9987, "wrapS": 10497, "wrapT": 10497 })]
    };
    (images, textures, samplers)
}

/// Appends `values` to `buffer` as a new bufferView plus accessor,
/// returning the accessor's index.
///
/// `count` is the number of *elements* (vertices, indices), not scalars.
#[allow(
    clippy::too_many_arguments,
    reason = "one parameter per glTF accessor field; a struct here would only move the same list"
)]
fn accessor_for<T: GltfScalar>(
    buffer: &mut Vec<u8>,
    views: &mut Vec<Value>,
    accessors: &mut Vec<Value>,
    values: impl Iterator<Item = T>,
    count: usize,
    element_type: &str,
    component_type: u32,
    target: u32,
    min_max: Option<([f32; 3], [f32; 3])>,
) -> usize {
    // Every accessor's offset must be a multiple of its component size
    // (4 here); pad rather than assume the previous view left us aligned.
    while !buffer.len().is_multiple_of(4) {
        buffer.push(0);
    }
    let offset = buffer.len();
    for value in values {
        value.write_le(buffer);
    }

    let view = views.len();
    let mut view_json = Map::new();
    view_json.insert("buffer".to_string(), json!(0));
    view_json.insert("byteOffset".to_string(), json!(offset));
    view_json.insert("byteLength".to_string(), json!(buffer.len() - offset));
    // Target `0` means "not a vertex or index buffer" — inverse bind
    // matrices and animation samplers, which strict validators reject if
    // given a target they do not belong to.
    if target != 0 {
        view_json.insert("target".to_string(), json!(target));
    }
    views.push(Value::Object(view_json));

    let mut accessor = Map::new();
    accessor.insert("bufferView".to_string(), json!(view));
    accessor.insert("componentType".to_string(), json!(component_type));
    accessor.insert("count".to_string(), json!(count));
    accessor.insert("type".to_string(), json!(element_type));
    if let Some((min, max)) = min_max {
        accessor.insert("min".to_string(), json!(min));
        accessor.insert("max".to_string(), json!(max));
    }
    accessors.push(Value::Object(accessor));
    accessors.len() - 1
}

/// The component-wise min and max of every vertex position, glTF's
/// required `POSITION` accessor bounds.
fn bounds(vertices: &[Vertex]) -> ([f32; 3], [f32; 3]) {
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for vertex in vertices {
        for axis in 0..3 {
            min[axis] = min[axis].min(vertex.position[axis]);
            max[axis] = max[axis].max(vertex.position[axis]);
        }
    }
    (min, max)
}

/// A scalar this writer can pack into a glTF buffer, little-endian as
/// the spec requires.
trait GltfScalar {
    /// Appends this value's little-endian bytes to `out`.
    fn write_le(self, out: &mut Vec<u8>);
}

impl GltfScalar for f32 {
    fn write_le(self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.to_le_bytes());
    }
}

impl GltfScalar for u32 {
    fn write_le(self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.to_le_bytes());
    }
}

impl GltfScalar for u16 {
    fn write_le(self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.to_le_bytes());
    }
}

/// A `data:` URI carrying `bytes` base64-encoded.
fn data_uri(mime: &str, bytes: &[u8]) -> String {
    format!("data:{mime};base64,{}", base64_encode(bytes))
}

/// Standard base64 (RFC 4648) with `=` padding.
///
/// Hand-rolled rather than pulled in as a dependency: encoding is a
/// dozen lines, this crate needs no decoder (the `gltf` crate brings its
/// own for reading), and the round-trip test proves it byte for byte —
/// a wrong implementation cannot survive its own importer.
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        // Pack up to three bytes into 24 bits, then read out four
        // six-bit groups; the tail is padded with `=` per missing byte.
        let b0 = u32::from(chunk[0]);
        let b1 = chunk.get(1).copied().map_or(0, u32::from);
        let b2 = chunk.get(2).copied().map_or(0, u32::from);
        let packed = (b0 << 16) | (b1 << 8) | b2;

        out.push(ALPHABET[(packed >> 18) as usize & 0x3f] as char);
        out.push(ALPHABET[(packed >> 12) as usize & 0x3f] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(packed >> 6) as usize & 0x3f] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[packed as usize & 0x3f] as char
        } else {
            '='
        });
    }
    out
}

/// Encodes tightly-packed RGBA8 `pixels` as PNG bytes.
///
/// The inverse of [`crate::import_texture_bytes`]'s decode step, so a
/// generator can write a real `.png` into a project's `assets/` folder
/// instead of keeping pixels in memory where nothing can browse them.
///
/// # Errors
///
/// [`AssetError::TextureExport`] if `pixels` is not exactly
/// `width * height * 4` bytes, or if PNG encoding fails.
pub fn encode_png_rgba8(width: u32, height: u32, pixels: &[u8]) -> Result<Vec<u8>, AssetError> {
    let expected = width as usize * height as usize * 4;
    if pixels.len() != expected {
        return Err(AssetError::TextureExport(format!(
            "expected {expected} bytes for {width}x{height} RGBA8, got {}",
            pixels.len()
        )));
    }
    let buffer = image::RgbaImage::from_raw(width, height, pixels.to_vec()).ok_or_else(|| {
        AssetError::TextureExport("pixel buffer did not fit the given dimensions".to_string())
    })?;

    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(buffer)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|err| AssetError::TextureExport(err.to_string()))?;
    Ok(png)
}

/// Wraps 16-bit mono PCM `samples` at `sample_rate` in a RIFF/WAVE
/// container, ready to write to a `.wav` file.
///
/// The inverse of [`crate::import_wav_bytes`], and the same reason: a
/// procedurally generated sound should end up as a file a person can
/// audition and replace, not bytes that exist only while the game runs.
///
/// # Errors
///
/// [`AssetError::AudioExport`] if `samples` is empty or `sample_rate` is
/// zero — either would produce a file no player accepts.
pub fn encode_wav_mono16(sample_rate: u32, samples: &[i16]) -> Result<Vec<u8>, AssetError> {
    if samples.is_empty() {
        return Err(AssetError::AudioExport("no samples to encode".to_string()));
    }
    if sample_rate == 0 {
        return Err(AssetError::AudioExport(
            "sample rate must be greater than zero".to_string(),
        ));
    }

    const CHANNELS: u16 = 1;
    const BITS_PER_SAMPLE: u16 = 16;
    let data_len = (samples.len() * 2) as u32;
    let byte_rate = sample_rate * u32::from(CHANNELS) * u32::from(BITS_PER_SAMPLE) / 8;
    let block_align = CHANNELS * BITS_PER_SAMPLE / 8;

    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    // Everything after this field: the 4-byte "WAVE" tag, the 24-byte
    // fmt chunk, the 8-byte data header, and the samples.
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&CHANNELS.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&BITS_PER_SAMPLE.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        wav.extend_from_slice(&sample.to_le_bytes());
    }
    Ok(wav)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{import_gltf_file, import_gltf_slice};

    fn triangle() -> (Vec<Vertex>, Vec<u32>) {
        let vertices = vec![
            Vertex {
                position: [0.0, 0.0, 0.0],
                normal: [0.0, 1.0, 0.0],
                uv: [0.0, 0.0],
            },
            Vertex {
                position: [1.0, 0.0, 0.0],
                normal: [0.0, 1.0, 0.0],
                uv: [1.0, 0.0],
            },
            Vertex {
                position: [0.0, 0.0, 2.0],
                normal: [0.0, 1.0, 0.0],
                uv: [0.0, 1.0],
            },
        ];
        (vertices, vec![0, 1, 2])
    }

    #[test]
    fn base64_matches_the_rfc_test_vectors() {
        assert_eq!(base64_encode(b"Man"), "TWFu");
        assert_eq!(base64_encode(b"Ma"), "TWE=");
        assert_eq!(base64_encode(b"M"), "TQ==");
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"light work."), "bGlnaHQgd29yay4=");
    }

    #[test]
    fn geometry_survives_a_round_trip_through_our_own_importer() {
        let (vertices, indices) = triangle();
        let bytes = export_gltf(
            &[ExportMesh {
                name: "tri",
                vertices: &vertices,
                indices: &indices,
                material: None,
            }],
            &[],
        )
        .expect("a valid triangle exports");

        let imported = import_gltf_slice(&bytes).expect("what we write, we must be able to read");
        assert_eq!(imported.meshes.len(), 1);
        let mesh = &imported.meshes[0];
        assert_eq!(mesh.name.as_deref(), Some("tri"));
        assert_eq!(mesh.indices, indices);
        assert_eq!(mesh.vertices.len(), vertices.len());
        for (written, read) in vertices.iter().zip(&mesh.vertices) {
            assert_eq!(written.position, read.position);
            assert_eq!(written.normal, read.normal);
            assert_eq!(written.uv, read.uv);
        }
    }

    #[test]
    fn material_and_texture_survive_a_round_trip() {
        let (vertices, indices) = triangle();
        let png = encode_png_rgba8(2, 2, &[255, 0, 0, 255].repeat(4)).expect("2x2 red encodes");
        let material = ExportMaterial {
            name: "leaf".to_string(),
            base_color_factor: [0.5, 1.0, 0.25, 1.0],
            metallic_factor: 0.0,
            roughness_factor: 0.8,
            base_color_texture: Some(ExportTexture::Embedded(png)),
            alpha_mode: ExportAlphaMode::Mask,
            alpha_cutoff: 0.4,
        };
        let bytes = export_gltf(
            &[ExportMesh {
                name: "card",
                vertices: &vertices,
                indices: &indices,
                material: Some(0),
            }],
            &[material],
        )
        .expect("a textured triangle exports");

        let imported = import_gltf_slice(&bytes).expect("import");
        assert_eq!(imported.materials.len(), 1);
        let read = &imported.materials[0];
        assert_eq!(read.name.as_deref(), Some("leaf"));
        assert_eq!(read.base_color_factor, [0.5, 1.0, 0.25, 1.0]);
        assert_eq!(read.alpha_mode, "MASK");
        assert!((read.alpha_cutoff - 0.4).abs() < 1e-6);
        assert_eq!(read.base_color_image, Some(0));

        assert_eq!(imported.images.len(), 1, "the PNG must come back decoded");
        assert_eq!(imported.images[0].width, 2);
        assert_eq!(imported.images[0].height, 2);
        assert_eq!(&imported.images[0].rgba8[..4], &[255, 0, 0, 255]);
    }

    #[test]
    fn an_external_texture_is_read_back_from_its_own_file() {
        // The point of external references: the `.png` beside the model
        // is the live source, so editing it changes what loads. This
        // writes a model and a red texture, reads it back, then rewrites
        // the texture green and reads again — without touching the
        // model.
        let dir = std::env::temp_dir().join("vge-gltf-external-texture");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("textures")).expect("temp dirs");
        std::fs::create_dir_all(dir.join("models")).expect("temp dirs");

        let texture_path = dir.join("textures/paint.png");
        std::fs::write(
            &texture_path,
            encode_png_rgba8(1, 1, &[255, 0, 0, 255]).expect("red"),
        )
        .expect("write texture");

        let (vertices, indices) = triangle();
        let bytes = export_gltf(
            &[ExportMesh {
                name: "tri",
                vertices: &vertices,
                indices: &indices,
                material: Some(0),
            }],
            &[ExportMaterial::new("paint").with_texture_file("../textures/paint.png")],
        )
        .expect("export");
        let model_path = dir.join("models/tri.gltf");
        std::fs::write(&model_path, &bytes).expect("write model");

        let first = import_gltf_file(&model_path).expect("import with the texture beside it");
        assert_eq!(first.images.len(), 1);
        assert_eq!(&first.images[0].rgba8[..4], &[255, 0, 0, 255]);

        // Repaint the texture only.
        std::fs::write(
            &texture_path,
            encode_png_rgba8(1, 1, &[0, 255, 0, 255]).expect("green"),
        )
        .expect("rewrite texture");
        let second = import_gltf_file(&model_path).expect("import again");
        assert_eq!(
            &second.images[0].rgba8[..4],
            &[0, 255, 0, 255],
            "editing the .png must change what the model loads",
        );

        // And the slice importer cannot do this, by design.
        assert!(
            import_gltf_slice(&bytes).is_err(),
            "an external reference needs a path; slice-only import must say so",
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn several_meshes_all_reach_the_default_scene() {
        let (vertices, indices) = triangle();
        let mesh = |name| ExportMesh {
            name,
            vertices: &vertices,
            indices: &indices,
            material: None,
        };
        let bytes =
            export_gltf(&[mesh("a"), mesh("b"), mesh("c")], &[]).expect("three meshes export");

        let imported = import_gltf_slice(&bytes).expect("import");
        // The importer walks the scene's nodes, so a mesh with no node
        // would silently vanish.
        assert_eq!(imported.meshes.len(), 3);
    }

    /// A two-joint skin: a root and one child above it.
    fn two_joint_skin() -> ExportSkin {
        ExportSkin {
            name: "rig".to_string(),
            joints: vec![
                ExportJoint {
                    name: "root".to_string(),
                    parent: None,
                    local_bind_transform: Transform::from_translation(Vec3::new(0.0, 1.0, 0.0)),
                    inverse_bind_matrix: Mat4::from_translation(Vec3::new(0.0, -1.0, 0.0)),
                },
                ExportJoint {
                    name: "tip".to_string(),
                    parent: Some(0),
                    local_bind_transform: Transform::from_translation(Vec3::new(0.0, 0.5, 0.0)),
                    inverse_bind_matrix: Mat4::from_translation(Vec3::new(0.0, -1.5, 0.0)),
                },
            ],
        }
    }

    fn skinned_triangle() -> (Vec<SkinnedVertex>, Vec<u32>) {
        let vertex = |y: f32, joint: u32| SkinnedVertex {
            position: [0.0, y, 0.0],
            normal: [0.0, 0.0, 1.0],
            uv: [0.0, 0.0],
            joints: [joint, 0, 0, 0],
            weights: [1.0, 0.0, 0.0, 0.0],
        };
        (
            vec![vertex(0.0, 0), vertex(1.0, 1), vertex(2.0, 1)],
            vec![0, 1, 2],
        )
    }

    #[test]
    fn a_skin_survives_a_round_trip() {
        let (vertices, indices) = skinned_triangle();
        let skin = two_joint_skin();
        let bytes = export_gltf_scene(&GltfScene {
            skinned_meshes: &[ExportSkinnedMesh {
                name: "body",
                vertices: &vertices,
                indices: &indices,
                material: None,
                skin: 0,
            }],
            skins: std::slice::from_ref(&skin),
            ..Default::default()
        })
        .expect("a skinned mesh exports");

        let imported = import_gltf_slice(&bytes).expect("and imports again");
        assert_eq!(imported.skeletons.len(), 1);
        let skeleton = &imported.skeletons[0];
        assert_eq!(skeleton.joints.len(), 2);
        assert_eq!(skeleton.joints[0].name.as_deref(), Some("root"));
        assert_eq!(
            skeleton.joints[1].parent,
            Some(0),
            "the hierarchy must survive, or every child joint detaches",
        );
        assert_eq!(
            skeleton.joints[1].local_bind_transform.translation,
            Vec3::new(0.0, 0.5, 0.0),
        );
        assert_eq!(
            skeleton.joints[0].inverse_bind_matrix, skin.joints[0].inverse_bind_matrix,
            "a wrong inverse bind matrix bends the mesh inside out",
        );

        let mesh = &imported.meshes[0];
        assert_eq!(mesh.skeleton, Some(0), "the mesh must find its skin");
        let weights = mesh.skin_weights.as_ref().expect("skin weights");
        assert_eq!(weights.joints.len(), vertices.len());
        assert_eq!(weights.joints[1], [1, 0, 0, 0]);
        assert_eq!(weights.weights[1], [1.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn an_animation_and_its_events_survive_a_round_trip() {
        let (vertices, indices) = skinned_triangle();
        let bytes = export_gltf_scene(&GltfScene {
            skinned_meshes: &[ExportSkinnedMesh {
                name: "body",
                vertices: &vertices,
                indices: &indices,
                material: None,
                skin: 0,
            }],
            skins: &[two_joint_skin()],
            animations: &[ExportAnimation {
                name: "walk".to_string(),
                skin: 0,
                channels: vec![
                    ExportChannel {
                        joint: 1,
                        times: vec![0.0, 0.5, 1.0],
                        values: ExportChannelValues::Rotation(vec![
                            Quat::IDENTITY,
                            Quat::from_rotation_x(0.5),
                            Quat::IDENTITY,
                        ]),
                    },
                    ExportChannel {
                        joint: 0,
                        times: vec![0.0, 1.0],
                        values: ExportChannelValues::Translation(vec![
                            Vec3::ZERO,
                            Vec3::new(0.0, 0.2, 0.0),
                        ]),
                    },
                ],
                events: vec![
                    ExportAnimationEvent {
                        time: 0.25,
                        name: "footstep".to_string(),
                    },
                    ExportAnimationEvent {
                        time: 0.75,
                        name: "footstep".to_string(),
                    },
                ],
            }],
            ..Default::default()
        })
        .expect("export");

        let imported = import_gltf_slice(&bytes).expect("import");
        assert_eq!(imported.animations.len(), 1);
        let clip = &imported.animations[0];
        assert_eq!(clip.name.as_deref(), Some("walk"));
        assert!((clip.duration - 1.0).abs() < 1e-6);

        // Channels are keyed by glTF node index; the skeleton's joints
        // carry the same indices, which is how a sampler finds them.
        let skeleton = &imported.skeletons[0];
        let tip = skeleton.joints[1].node_index;
        let root = skeleton.joints[0].node_index;
        let tip_channels = clip.channels.get(&tip).expect("the tip is animated");
        assert_eq!(
            tip_channels.rotation.as_ref().map(|keys| keys.times.len()),
            Some(3),
        );
        assert!(
            clip.channels
                .get(&root)
                .and_then(|channels| channels.translation.as_ref())
                .is_some(),
            "the root's translation channel must survive too",
        );

        assert_eq!(
            clip.events.len(),
            2,
            "a clip must carry its own events, or timing lives in game code",
        );
        assert_eq!(clip.events[0].name, "footstep");
        assert!((clip.events[0].time - 0.25).abs() < 1e-6);
    }

    #[test]
    fn a_clip_without_events_imports_with_none() {
        let (vertices, indices) = skinned_triangle();
        let bytes = export_gltf_scene(&GltfScene {
            skinned_meshes: &[ExportSkinnedMesh {
                name: "body",
                vertices: &vertices,
                indices: &indices,
                material: None,
                skin: 0,
            }],
            skins: &[two_joint_skin()],
            animations: &[ExportAnimation {
                name: "idle".to_string(),
                skin: 0,
                channels: vec![ExportChannel {
                    joint: 0,
                    times: vec![0.0],
                    values: ExportChannelValues::Scale(vec![Vec3::ONE]),
                }],
                events: Vec::new(),
            }],
            ..Default::default()
        })
        .expect("export");
        let imported = import_gltf_slice(&bytes).expect("import");
        assert!(imported.animations[0].events.is_empty());
    }

    #[test]
    fn a_vertex_weighted_to_a_missing_joint_is_refused() {
        let (mut vertices, indices) = skinned_triangle();
        vertices[0].joints = [7, 0, 0, 0];
        let error = export_gltf_scene(&GltfScene {
            skinned_meshes: &[ExportSkinnedMesh {
                name: "body",
                vertices: &vertices,
                indices: &indices,
                material: None,
                skin: 0,
            }],
            skins: &[two_joint_skin()],
            ..Default::default()
        })
        .expect_err("a weight on a joint that does not exist must not be written");
        assert!(matches!(error, AssetError::GltfExport(_)));
    }

    #[test]
    fn a_joint_parented_after_itself_is_refused() {
        let mut skin = two_joint_skin();
        skin.joints[0].parent = Some(1);
        let (vertices, indices) = skinned_triangle();
        assert!(
            export_gltf_scene(&GltfScene {
                skinned_meshes: &[ExportSkinnedMesh {
                    name: "body",
                    vertices: &vertices,
                    indices: &indices,
                    material: None,
                    skin: 0,
                }],
                skins: &[skin],
                ..Default::default()
            })
            .is_err()
        );
    }

    #[test]
    fn mismatched_keyframe_counts_are_refused() {
        let (vertices, indices) = skinned_triangle();
        assert!(
            export_gltf_scene(&GltfScene {
                skinned_meshes: &[ExportSkinnedMesh {
                    name: "body",
                    vertices: &vertices,
                    indices: &indices,
                    material: None,
                    skin: 0,
                }],
                skins: &[two_joint_skin()],
                animations: &[ExportAnimation {
                    name: "broken".to_string(),
                    skin: 0,
                    channels: vec![ExportChannel {
                        joint: 0,
                        times: vec![0.0, 1.0],
                        values: ExportChannelValues::Translation(vec![Vec3::ZERO]),
                    }],
                    events: Vec::new(),
                }],
                ..Default::default()
            })
            .is_err()
        );
    }

    #[test]
    fn an_out_of_range_index_is_refused() {
        let (vertices, _) = triangle();
        let error = export_gltf(
            &[ExportMesh {
                name: "broken",
                vertices: &vertices,
                indices: &[0, 1, 9],
                material: None,
            }],
            &[],
        )
        .expect_err("an index past the vertex list must not be written");
        assert!(matches!(error, AssetError::GltfExport(_)));
    }

    #[test]
    fn a_partial_triangle_is_refused() {
        let (vertices, _) = triangle();
        assert!(
            export_gltf(
                &[ExportMesh {
                    name: "broken",
                    vertices: &vertices,
                    indices: &[0, 1],
                    material: None,
                }],
                &[],
            )
            .is_err()
        );
    }

    #[test]
    fn an_empty_mesh_is_refused() {
        assert!(
            export_gltf(
                &[ExportMesh {
                    name: "empty",
                    vertices: &[],
                    indices: &[],
                    material: None,
                }],
                &[],
            )
            .is_err()
        );
        assert!(export_gltf(&[], &[]).is_err());
    }

    #[test]
    fn a_missing_material_index_is_refused() {
        let (vertices, indices) = triangle();
        assert!(
            export_gltf(
                &[ExportMesh {
                    name: "tri",
                    vertices: &vertices,
                    indices: &indices,
                    material: Some(3),
                }],
                &[ExportMaterial::new("only one")],
            )
            .is_err()
        );
    }

    #[test]
    fn png_round_trips_through_the_texture_importer() {
        let pixels: Vec<u8> = (0..4 * 4 * 4).map(|i| (i % 251) as u8).collect();
        let png = encode_png_rgba8(4, 4, &pixels).expect("encode");
        let imported = crate::import_texture_bytes(&png).expect("decode");
        assert_eq!(imported.width, 4);
        assert_eq!(imported.height, 4);
        assert_eq!(
            imported.mip_levels.first().map(Vec::as_slice),
            Some(pixels.as_slice()),
            "level 0 must be the pixels we encoded, byte for byte",
        );
    }

    #[test]
    fn png_rejects_a_mismatched_pixel_count() {
        assert!(encode_png_rgba8(4, 4, &[0; 16]).is_err());
    }

    #[test]
    fn wav_round_trips_through_the_audio_importer() {
        let samples: Vec<i16> = (0..128).map(|i| (i * 200 - 12000) as i16).collect();
        let wav = encode_wav_mono16(22_050, &samples).expect("encode");
        let imported = crate::import_wav_bytes(&wav).expect("decode");
        assert_eq!(imported.sample_rate, 22_050);
        assert_eq!(imported.channels, 1);
        assert_eq!(imported.samples.len(), samples.len());
    }

    #[test]
    fn wav_rejects_empty_or_rateless_input() {
        assert!(encode_wav_mono16(22_050, &[]).is_err());
        assert!(encode_wav_mono16(0, &[1, 2, 3]).is_err());
    }
}
