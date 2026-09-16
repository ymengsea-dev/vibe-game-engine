//! Turns the procedural generators into a project on disk.
//!
//! Running [`bake`] writes:
//!
//! ```text
//! project.ron                     the manifest the studio opens
//! assets/textures/*.png           grass, bark, leaf, stone, skin
//! assets/models/*.gltf            terrain, trunks, canopies, rocks, character
//! assets/audio/footstep.wav       the walk-cycle footfall
//! scenes/island.ron               terrain and every scattered prop, placed
//! ```
//!
//! plus a `.meta` sidecar beside each asset holding its stable
//! [`engine::asset::AssetId`]. The scene refers to props by those ids,
//! so renaming a file does not break the level.
//!
//! ## Why variants instead of one mesh per prop
//!
//! The generators can produce a unique trunk for every tree, and the old
//! in-memory version did exactly that — 110 unique trunk meshes. As
//! files that would be 110 models nobody would ever open. Baking a
//! handful of variants and scattering *those* is both what a real game
//! ships and what lets the renderer batch them.
//!
//! ## What is not baked
//!
//! The character: it is skinned, and the exporter writes static meshes
//! only. Lights, camera and physics colliders: the scene format carries
//! neither, so the game still sets those up in code.

use std::path::{Path, PathBuf};

use engine::asset::{
    AssetMeta, ExportAnimation, ExportMaterial, ExportMesh, ExportSkin, ExportSkinnedMesh,
    GltfScene, encode_png_rgba8, encode_wav_mono16, export_gltf, export_gltf_scene,
};
use engine::prelude::{
    AssetRef, AudioEmitterData, BodyKind, ColliderData, ColliderShape, MeshRendererData, Scene,
    SceneEntity, TransformData, Vertex,
};
use engine::utils::Transform;
use engine_project::Project;

use crate::world::{Placement, Rng};
use crate::{character, world};

/// How many trunk/canopy variants to bake. Three of each reads as
/// varied at this density without turning the model folder into noise.
const TREE_VARIANTS: usize = 3;
/// How many boulder variants to bake.
const ROCK_VARIANTS: usize = 4;
/// Trees scattered into the baked scene.
const TREE_COUNT: usize = 110;
/// Boulders scattered into the baked scene.
const ROCK_COUNT: usize = 55;
/// Scatter seeds. Shared with nothing — the scene file is the output, so
/// these only ever need to be stable within one bake.
const TREE_SEED: u64 = 0xA11CE;
const ROCK_SEED: u64 = 0xB0B;
/// Seed for per-variant mesh shape.
const SHAPE_SEED: u64 = 0xC0FFEE;

/// Everything that can go wrong while baking.
#[derive(Debug, thiserror::Error)]
pub enum BakeError {
    /// A file could not be written, or a directory created.
    #[error("writing {path}: {message}")]
    Io {
        /// The path being written.
        path: String,
        /// The underlying error.
        message: String,
    },
    /// An exporter refused the generated data — a generator bug.
    #[error("encoding {what}: {message}")]
    Encode {
        /// Which asset failed.
        what: String,
        /// The underlying error.
        message: String,
    },
    /// The project manifest or scene could not be written.
    #[error("project: {0}")]
    Project(String),
}

/// What one [`bake`] produced, for the caller to report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BakeReport {
    /// Texture files written.
    pub textures: usize,
    /// Model files written.
    pub models: usize,
    /// Audio files written.
    pub audio: usize,
    /// Entities placed in the baked scene.
    pub scene_entities: usize,
}

/// Generates every asset and writes the project at `root`.
///
/// Idempotent: re-baking overwrites the files but keeps each asset's
/// `.meta` sidecar, so ids stay stable and a scene that references them
/// keeps working. Safe to run after tweaking a generator.
///
/// # Errors
///
/// [`BakeError`] on any filesystem failure, or if a generator produced
/// geometry the exporter refuses (see
/// [`engine::asset::export_gltf`]).
pub fn bake(root: &Path) -> Result<BakeReport, BakeError> {
    let project = open_or_create_project(root)?;
    let textures_dir = project.assets_dir().join("textures");
    let models_dir = project.assets_dir().join("models");
    let audio_dir = project.assets_dir().join("audio");
    for dir in [&textures_dir, &models_dir, &audio_dir] {
        create_dir(dir)?;
    }

    // --- textures ----------------------------------------------------
    // Written first, and referenced by the models rather than copied
    // into them: the `.png` in `assets/textures/` stays the live source,
    // so repainting one and restarting shows the change without a
    // re-bake.
    write_texture(&textures_dir, "grass", world::grass_texture())?;
    write_texture(&textures_dir, "bark", world::bark_texture())?;
    write_texture(&textures_dir, "leaf", world::foliage_texture())?;
    write_texture(&textures_dir, "stone", world::stone_texture())?;

    // --- models ------------------------------------------------------
    let (terrain_vertices, terrain_indices) = world::terrain_mesh();
    let terrain = write_model(
        &models_dir,
        "terrain",
        &terrain_vertices,
        &terrain_indices,
        ExportMaterial {
            roughness_factor: 0.95,
            ..ExportMaterial::new("grass").with_texture_file(texture_uri("grass"))
        },
    )?;

    let mut shape_rng = Rng::new(SHAPE_SEED);
    let mut trunks = Vec::with_capacity(TREE_VARIANTS);
    let mut canopies = Vec::with_capacity(TREE_VARIANTS);
    for variant in 0..TREE_VARIANTS {
        let (vertices, indices) = world::trunk_mesh(&mut shape_rng);
        trunks.push(write_model(
            &models_dir,
            &format!("trunk_{:02}", variant + 1),
            &vertices,
            &indices,
            ExportMaterial {
                roughness_factor: 0.9,
                ..ExportMaterial::new("bark").with_texture_file(texture_uri("bark"))
            },
        )?);

        let (vertices, indices) = world::canopy_mesh(&mut shape_rng);
        canopies.push(write_model(
            &models_dir,
            &format!("canopy_{:02}", variant + 1),
            &vertices,
            &indices,
            // Alpha-masked: the texture's alpha is the leaf shape, and
            // without the mask the canopy reads as flat cards.
            ExportMaterial::new("leaf")
                .with_texture_file(texture_uri("leaf"))
                .with_alpha_mask(0.5),
        )?);
    }

    let mut rocks = Vec::with_capacity(ROCK_VARIANTS);
    for variant in 0..ROCK_VARIANTS {
        let (vertices, indices) = world::rock_mesh(&mut shape_rng);
        rocks.push(write_model(
            &models_dir,
            &format!("rock_{:02}", variant + 1),
            &vertices,
            &indices,
            ExportMaterial {
                roughness_factor: 0.8,
                ..ExportMaterial::new("stone").with_texture_file(texture_uri("stone"))
            },
        )?);
    }

    // --- the character ------------------------------------------------
    // Skeleton, skin weights and the walk clip, in one file. The clip
    // carries its own footstep events, so the sound cannot drift out of
    // sync with the motion.
    write_texture(&textures_dir, "skin", world::flat_texture([214, 122, 96]))?;
    let skeleton = character::skeleton();
    let clip = character::walk_clip();
    let (skin_vertices, skin_indices) = character::mesh();
    let character_bytes = export_gltf_scene(&GltfScene {
        skinned_meshes: &[ExportSkinnedMesh {
            name: "character",
            vertices: &skin_vertices,
            indices: &skin_indices,
            material: Some(0),
            skin: 0,
        }],
        skins: &[ExportSkin::from_imported(&skeleton)],
        animations: &[ExportAnimation::from_imported(&clip, &skeleton, 0)],
        materials: &[ExportMaterial {
            roughness_factor: 0.75,
            ..ExportMaterial::new("skin").with_texture_file(texture_uri("skin"))
        }],
        ..Default::default()
    })
    .map_err(|err| BakeError::Encode {
        what: "character.gltf".to_string(),
        message: err.to_string(),
    })?;
    write_asset(&models_dir.join("character.gltf"), &character_bytes)?;

    // --- audio -------------------------------------------------------
    let (rate, samples) = world::footstep_samples();
    let wav = encode_wav_mono16(rate, &samples).map_err(|err| BakeError::Encode {
        what: "footstep.wav".to_string(),
        message: err.to_string(),
    })?;
    write_asset(&audio_dir.join("footstep.wav"), &wav)?;

    // Ambience, authored into the scene rather than started by code: the
    // wind is a thing that exists at a place in the level, which is
    // exactly what an audio emitter is for.
    let (rate, samples) = world::wind_samples();
    let wind_wav = encode_wav_mono16(rate, &samples).map_err(|err| BakeError::Encode {
        what: "wind.wav".to_string(),
        message: err.to_string(),
    })?;
    let wind_id = write_asset(&audio_dir.join("wind.wav"), &wind_wav)?;

    // --- the scene ---------------------------------------------------
    let scene = build_scene(&terrain, &trunks, &canopies, &rocks, &wind_id);
    let scene_path = project.scenes_dir().join("island.ron");
    scene
        .save_to_file(&scene_path)
        .map_err(|err| BakeError::Project(err.to_string()))?;

    Ok(BakeReport {
        textures: 5,
        models: 2 + trunks.len() + canopies.len() + rocks.len(),
        audio: 2,
        scene_entities: scene.entities.len(),
    })
}

/// Collider dimensions for a tree trunk, in unscaled local units. The
/// entity's scale is applied when the body is built.
const TRUNK_COLLIDER: (f32, f32) = (1.6, 0.35);
/// Radius of a boulder's collider, in unscaled local units.
const ROCK_COLLIDER_RADIUS: f32 = 0.55;

/// A model written to disk, identified by the id its `.meta` carries.
struct BakedModel {
    id: String,
}

impl BakedModel {
    /// This model as a scene reference.
    ///
    /// Mesh and material point at the same file on purpose: a baked
    /// `.gltf` carries its material and its texture inside it, so the
    /// material is not a separate asset to resolve.
    fn renderer(&self) -> MeshRendererData {
        MeshRendererData {
            mesh: AssetRef {
                id: self.id.clone(),
            },
            material: AssetRef {
                id: self.id.clone(),
            },
        }
    }
}

/// Opens the project at `root`, creating it if this is the first bake.
fn open_or_create_project(root: &Path) -> Result<Project, BakeError> {
    let mut project = Project::open_or_create(root, "Island")
        .map_err(|err| BakeError::Project(err.to_string()))?;
    // Point the manifest at the scene this baker writes, not at the
    // empty `main.ron` a fresh project is scaffolded with.
    let main_scene = PathBuf::from("scenes").join("island.ron");
    if project.manifest().main_scene != main_scene {
        project.manifest_mut().main_scene = main_scene;
        project
            .save_manifest()
            .map_err(|err| BakeError::Project(err.to_string()))?;
    }
    Ok(project)
}

/// Encodes `(width, height, rgba)` as a PNG and writes it, with its
/// `.meta` sidecar.
///
/// Nothing is returned: models reach these files by relative URI rather
/// than by id or by copy, so the file on disk is the only thing that
/// matters. The sidecar still gives the texture a stable identity for
/// the asset browser.
fn write_texture(dir: &Path, name: &str, texture: (u32, u32, Vec<u8>)) -> Result<(), BakeError> {
    let (width, height, pixels) = texture;
    let png = encode_png_rgba8(width, height, &pixels).map_err(|err| BakeError::Encode {
        what: format!("{name}.png"),
        message: err.to_string(),
    })?;
    let path = dir.join(format!("{name}.png"));
    write_asset(&path, &png)?;
    Ok(())
}

/// The URI a model uses to reach a texture, relative to the model's own
/// directory — `assets/models/x.gltf` reaching `assets/textures/y.png`.
fn texture_uri(name: &str) -> String {
    format!("../textures/{name}.png")
}

/// Exports one mesh as a self-contained `.gltf` and writes it.
fn write_model(
    dir: &Path,
    name: &str,
    vertices: &[Vertex],
    indices: &[u32],
    material: ExportMaterial,
) -> Result<BakedModel, BakeError> {
    let bytes = export_gltf(
        &[ExportMesh {
            name,
            vertices,
            indices,
            material: Some(0),
        }],
        &[material],
    )
    .map_err(|err| BakeError::Encode {
        what: format!("{name}.gltf"),
        message: err.to_string(),
    })?;
    let path = dir.join(format!("{name}.gltf"));
    let id = write_asset(&path, &bytes)?;
    Ok(BakedModel { id })
}

/// Writes `bytes` to `path` and returns the asset's stable id, creating
/// the `.meta` sidecar on the first bake and reusing it afterwards.
fn write_asset(path: &Path, bytes: &[u8]) -> Result<String, BakeError> {
    std::fs::write(path, bytes).map_err(|err| BakeError::Io {
        path: path.display().to_string(),
        message: err.to_string(),
    })?;
    // After the write, so the recorded content hash matches what is now
    // on disk and the editor does not see a stale-import warning.
    let meta = AssetMeta::load_or_create(path).map_err(|err| BakeError::Io {
        path: path.display().to_string(),
        message: err.to_string(),
    })?;
    Ok(meta.id.to_string())
}

/// Creates `dir` (and parents), tolerating one that already exists.
fn create_dir(dir: &Path) -> Result<(), BakeError> {
    std::fs::create_dir_all(dir).map_err(|err| BakeError::Io {
        path: dir.display().to_string(),
        message: err.to_string(),
    })
}

/// Places the terrain and every scattered prop into a [`Scene`].
///
/// Scattering happens here, at bake time, rather than at startup: the
/// level becomes data a person can open, edit and version, instead of a
/// number that has to be re-rolled identically on every machine.
fn build_scene(
    terrain: &BakedModel,
    trunks: &[BakedModel],
    canopies: &[BakedModel],
    rocks: &[BakedModel],
    wind_id: &str,
) -> Scene {
    let mut entities = vec![
        SceneEntity {
            name: Some("Wind".to_string()),
            transform: Some(TransformData::from(Transform::from_translation(
                glam::Vec3::new(0.0, 6.0, 0.0),
            ))),
            // Looping ambience over the middle of the island. Data, not
            // code: moving it or muting it is a scene edit.
            audio_emitter: Some(AudioEmitterData {
                sound: AssetRef {
                    id: wind_id.to_string(),
                },
                autoplay: true,
                looping: true,
                gain: 0.5,
                radius: 60.0,
            }),
            ..Default::default()
        },
        SceneEntity {
            name: Some("Terrain".to_string()),
            transform: Some(TransformData::from(Transform::IDENTITY)),
            mesh_renderer: Some(terrain.renderer()),
            is_static: true,
            // Collide against the terrain model itself, not against a
            // separately generated approximation: whatever the hills look
            // like is what the player walks on.
            collider: Some(ColliderData {
                shape: ColliderShape::TriMesh {
                    mesh: AssetRef {
                        id: terrain.id.clone(),
                    },
                },
                body: BodyKind::Fixed,
            }),
            ..Default::default()
        },
    ];

    for (index, placement) in world::scatter(TREE_SEED, TREE_COUNT, 0.5, 0.82)
        .into_iter()
        .enumerate()
    {
        let transform = Some(TransformData::from(transform_of(placement)));
        // Deterministic variant choice: the same scatter always dresses
        // the same tree with the same trunk, so a re-bake is a no-op
        // rather than a reshuffle.
        let variant = index % trunks.len();
        entities.push(SceneEntity {
            name: Some(format!("Tree {:03} Trunk", index + 1)),
            transform,
            mesh_renderer: Some(trunks[variant].renderer()),
            is_static: true,
            collider: Some(ColliderData {
                shape: ColliderShape::Cylinder {
                    half_height: TRUNK_COLLIDER.0,
                    radius: TRUNK_COLLIDER.1,
                },
                body: BodyKind::Fixed,
            }),
            ..Default::default()
        });
        entities.push(SceneEntity {
            name: Some(format!("Tree {:03} Canopy", index + 1)),
            transform,
            mesh_renderer: Some(canopies[variant].renderer()),
            is_static: true,
            ..Default::default()
        });
    }

    for (index, placement) in world::scatter(ROCK_SEED, ROCK_COUNT, 0.2, 0.6)
        .into_iter()
        .enumerate()
    {
        entities.push(SceneEntity {
            name: Some(format!("Rock {:03}", index + 1)),
            transform: Some(TransformData::from(transform_of(placement))),
            mesh_renderer: Some(rocks[index % rocks.len()].renderer()),
            is_static: true,
            collider: Some(ColliderData {
                shape: ColliderShape::Ball {
                    radius: ROCK_COLLIDER_RADIUS,
                },
                body: BodyKind::Fixed,
            }),
            ..Default::default()
        });
    }

    Scene {
        entities,
        ..Default::default()
    }
}

/// The world transform for a scattered prop.
fn transform_of(placement: Placement) -> Transform {
    Transform {
        translation: placement.position,
        rotation: glam::Quat::from_rotation_y(placement.yaw),
        scale: glam::Vec3::splat(placement.scale),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bake into a throwaway directory, so the test never touches the
    /// checked-in project.
    fn bake_into_temp(name: &str) -> (PathBuf, BakeReport) {
        let root = std::env::temp_dir().join(format!("vge-island-bake-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("temp dir");
        let report = bake(&root).expect("baking a fresh project must succeed");
        (root, report)
    }

    #[test]
    fn bake_writes_a_complete_project() {
        let (root, report) = bake_into_temp("complete");

        assert!(root.join("project.ron").is_file(), "the studio needs this");
        assert!(root.join("scenes/island.ron").is_file());
        for texture in ["grass", "bark", "leaf", "stone"] {
            let path = root.join(format!("assets/textures/{texture}.png"));
            assert!(path.is_file(), "{texture}.png should exist");
            assert!(
                AssetMeta::sidecar_path(&path).is_file(),
                "every asset needs a stable id",
            );
        }
        assert!(root.join("assets/models/terrain.gltf").is_file());
        assert!(root.join("assets/models/trunk_01.gltf").is_file());
        assert!(root.join("assets/models/rock_04.gltf").is_file());
        assert!(root.join("assets/audio/footstep.wav").is_file());
        assert!(root.join("assets/audio/wind.wav").is_file());

        assert_eq!(report.textures, 5);
        assert_eq!(report.models, 2 + TREE_VARIANTS * 2 + ROCK_VARIANTS);
        assert_eq!(report.audio, 2);
        // The wind emitter, terrain, a trunk and a canopy per tree, and
        // every rock.
        assert_eq!(report.scene_entities, 2 + TREE_COUNT * 2 + ROCK_COUNT);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn every_baked_model_imports_again() {
        // The exporter has its own round-trip test on a triangle; this
        // one runs real generated geometry through the same path, which
        // is where a degenerate mesh or a bad index would show up.
        let (root, _) = bake_into_temp("import");
        let models = root.join("assets/models");
        let mut checked = 0;
        for entry in std::fs::read_dir(&models).expect("models dir") {
            let path = entry.expect("dir entry").path();
            if path.extension().is_none_or(|ext| ext != "gltf") {
                continue;
            }
            let imported = engine::asset::import_gltf_file(&path)
                .unwrap_or_else(|err| panic!("{} must import: {err}", path.display()));
            assert!(
                !imported.meshes.is_empty(),
                "{} has no mesh",
                path.display()
            );
            assert!(
                !imported.meshes[0].vertices.is_empty(),
                "{} has an empty mesh",
                path.display(),
            );
            assert_eq!(
                imported.images.len(),
                1,
                "{} should carry its texture",
                path.display(),
            );
            checked += 1;
        }
        assert_eq!(checked, 2 + TREE_VARIANTS * 2 + ROCK_VARIANTS);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_baked_scene_references_real_assets() {
        let (root, _) = bake_into_temp("scene");
        let scene = Scene::load_from_file(&root.join("scenes/island.ron")).expect("scene loads");

        // Collect every id the assets actually have, then check the
        // scene only names those — a scene pointing at an id no file
        // carries would load as an empty island.
        let mut known = Vec::new();
        for dir in ["assets/models", "assets/textures", "assets/audio"] {
            for entry in std::fs::read_dir(root.join(dir)).expect("asset dir") {
                let path = entry.expect("dir entry").path();
                if path.extension().is_some_and(|ext| ext == "meta") {
                    continue;
                }
                let meta = AssetMeta::load_or_create(&path).expect("meta");
                known.push(meta.id.to_string());
            }
        }

        for entity in &scene.entities {
            // Not every entity is a prop: the wind is a sound at a
            // place. Whatever kind of asset an entity names, the asset
            // has to exist.
            if let Some(renderer) = entity.mesh_renderer.as_ref() {
                assert!(
                    known.contains(&renderer.mesh.id),
                    "{:?} references an unknown mesh",
                    entity.name,
                );
            } else if let Some(emitter) = entity.audio_emitter.as_ref() {
                assert!(
                    known.contains(&emitter.sound.id),
                    "{:?} references an unknown sound",
                    entity.name,
                );
            } else {
                panic!("{:?} references nothing at all", entity.name);
            }
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn every_placed_prop_carries_a_collider() {
        // A scene of geometry with no bodies is scenery you walk
        // through — the bug this task exists to remove.
        let (root, _) = bake_into_temp("colliders");
        let scene = Scene::load_from_file(&root.join("scenes/island.ron")).expect("scene loads");

        let terrain = scene
            .entities
            .iter()
            .find(|entity| entity.name.as_deref() == Some("Terrain"))
            .expect("terrain entity");
        let collider = terrain.collider.as_ref().expect("terrain has a body");
        match &collider.shape {
            ColliderShape::TriMesh { mesh } => assert_eq!(
                Some(&mesh.id),
                terrain
                    .mesh_renderer
                    .as_ref()
                    .map(|renderer| &renderer.mesh.id),
                "terrain must collide against the model it draws",
            ),
            other => panic!("terrain should be a trimesh, got {other:?}"),
        }

        let trunks = scene
            .entities
            .iter()
            .filter(|entity| {
                entity
                    .name
                    .as_deref()
                    .is_some_and(|name| name.ends_with("Trunk"))
            })
            .count();
        assert_eq!(trunks, TREE_COUNT);

        for entity in &scene.entities {
            let Some(name) = entity.name.as_deref() else {
                continue;
            };
            // An emitter is not a prop — there is nothing to walk into.
            if entity.mesh_renderer.is_none() {
                assert!(
                    entity.audio_emitter.is_some(),
                    "{name} is neither a prop nor a sound",
                );
                continue;
            }
            if name.ends_with("Canopy") {
                assert!(
                    entity.collider.is_none(),
                    "leaves should not be solid: {name}",
                );
            } else {
                assert!(entity.collider.is_some(), "{name} has no collider");
            }
        }

        // And the whole thing must survive validation, which checks
        // every collider's dimensions.
        scene.validate().expect("a baked scene must be valid");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn models_reference_their_textures_by_path() {
        // The point of external references: the .png in assets/textures
        // is the live source. If a model embedded a copy instead,
        // repainting the texture would do nothing until a re-bake.
        let (root, _) = bake_into_temp("external-textures");
        let model = root.join("assets/models/terrain.gltf");
        let text = std::fs::read_to_string(&model).expect("read model");
        assert!(
            text.contains("../textures/grass.png"),
            "the model should point at the texture file",
        );
        assert!(
            !text.contains("image/png"),
            "and should not carry an embedded copy of it",
        );

        // It must still import — through the path-aware importer, which
        // is what can follow that reference.
        let imported = engine::asset::import_gltf_file(&model).expect("import");
        assert_eq!(imported.images.len(), 1, "the texture must resolve");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_baked_project_loads_the_way_the_game_loads_it() {
        // Everything `Island::setup` does short of the GPU: index the
        // assets, read the scene, and check every reference resolves.
        // If this passes and the game still shows nothing, the problem
        // is rendering, not the project.
        let (root, report) = bake_into_temp("load");
        let library = engine::prelude::AssetLibrary::index(&root.join("assets"))
            .expect("a baked project must index");
        assert_eq!(library.len(), 19, "5 textures, 12 models, 2 sounds");

        let scene = Scene::load_from_file(&root.join("scenes/island.ron")).expect("scene loads");
        assert_eq!(scene.entities.len(), report.scene_entities);
        for entity in &scene.entities {
            // Audio emitters are scene entities too, but intentionally have
            // no mesh. The runtime loads their sound reference through the
            // same indexed project, so only renderable entities need a mesh
            // lookup here.
            let Some(renderer) = entity.mesh_renderer.as_ref() else {
                assert!(
                    entity.audio_emitter.is_some(),
                    "entity has no renderable or audio data"
                );
                continue;
            };
            assert!(
                library.path_for(&renderer.mesh.id).is_some(),
                "{:?} references an asset the library cannot find",
                entity.name,
            );
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn terrain_collider_matches_the_loaded_mesh() {
        // The collider is built from the terrain *model*, so what you
        // walk on is what you see. Compare the geometry the collider
        // would load against the mesh the entity draws.
        let (root, _) = bake_into_temp("collider-match");
        let library = engine::prelude::AssetLibrary::index(&root.join("assets")).expect("index");
        let scene = Scene::load_from_file(&root.join("scenes/island.ron")).expect("scene");

        let terrain = scene
            .entities
            .iter()
            .find(|entity| entity.name.as_deref() == Some("Terrain"))
            .expect("terrain");
        let ColliderShape::TriMesh { mesh } =
            &terrain.collider.as_ref().expect("terrain has a body").shape
        else {
            panic!("terrain should collide as a trimesh");
        };

        let (collision_vertices, collision_indices) =
            engine::prelude::load_mesh_geometry(&library, mesh)
                .expect("loads")
                .expect("the project carries the terrain");
        let (rendered_vertices, rendered_indices) = world::terrain_mesh();
        assert_eq!(
            collision_vertices.len(),
            rendered_vertices.len(),
            "collision geometry must be the rendered geometry, not an approximation",
        );
        assert_eq!(collision_indices.len(), rendered_indices.len());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_character_bakes_with_its_skeleton_clip_and_events() {
        // The last thing the game built at runtime. If the skeleton,
        // the weights or the events do not survive the file, the
        // character either does not move or moves silently.
        let (root, _) = bake_into_temp("character");
        let path = root.join("assets/models/character.gltf");
        let imported = engine::asset::import_gltf_file(&path).expect("character imports");

        let skeleton = imported.skeletons.first().expect("a skeleton");
        assert_eq!(skeleton.joints.len(), character::skeleton().joints.len());
        assert_eq!(
            imported.meshes[0].skeleton,
            Some(0),
            "the mesh must be bound to the skin",
        );
        let weights = imported.meshes[0]
            .skin_weights
            .as_ref()
            .expect("skin weights");
        assert_eq!(weights.joints.len(), imported.meshes[0].vertices.len());

        let clip = imported.animations.first().expect("the walk clip");
        assert_eq!(clip.name.as_deref(), Some("walk"));
        assert!((clip.duration - character::WALK_DURATION).abs() < 1e-5);
        assert_eq!(
            clip.channels.len(),
            character::walk_clip().channels.len(),
            "every animated joint must survive",
        );
        assert_eq!(
            clip.events.len(),
            character::footstep_times().len(),
            "footsteps belong to the clip, not to game code",
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn re_baking_keeps_asset_ids_stable() {
        // Ids live in `.meta` sidecars and must survive a re-bake, or
        // every scene referencing them breaks the moment someone tunes
        // a generator.
        let (root, _) = bake_into_temp("stable");
        let texture = root.join("assets/textures/grass.png");
        let first = AssetMeta::load_or_create(&texture).expect("meta").id;

        bake(&root).expect("re-bake");
        let second = AssetMeta::load_or_create(&texture).expect("meta").id;
        assert_eq!(first, second);

        let _ = std::fs::remove_dir_all(&root);
    }
}
