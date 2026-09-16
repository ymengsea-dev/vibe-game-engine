//! Loading a scene's assets at runtime, from a project on disk.
//!
//! The editor resolves asset references through its import cache; a
//! shipped game has no editor, so it needs its own path from "this
//! entity references asset `3fa8…`" to "these vertices are on the GPU".
//! That path is here, in the engine, rather than in each game: it was
//! written once inside `apps/player` and would have been copied verbatim
//! into the next game that needed it.
//!
//! ## Two pieces
//!
//! [`AssetLibrary`] answers *where* — it walks a project's `assets/`
//! folder, reads the `.meta` sidecars, and maps each asset id to its
//! file. [`RuntimeResolver`] answers *what* — it implements
//! [`crate::SceneResolver`] by importing that file and uploading it,
//! caching uploads so a scene placing one model 110 times pays for one.
//!
//! ## Files, not bytes
//!
//! Models are imported with [`engine_asset::import_gltf_file`], which
//! resolves external texture references relative to the model's own
//! directory. That is what makes a `.png` sitting in `assets/textures/`
//! the live source for a material: repaint it, restart, see it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use engine_asset::{
    AssetId, AssetMeta, Bundle, ImportedGltf, ImportedMaterial, import_gltf_file,
    import_gltf_slice_with, import_texture_bytes,
};
use engine_ecs::components::MeshRenderer;
use engine_renderer::{
    AlphaMode, GpuContext, Material, MaterialBinding, Mesh, Pipeline, RenderAssets, UvRect,
};
use engine_utils::AssetHandle;

use crate::Scene;
use crate::error::SceneError;
use crate::format::AssetRef;
use crate::resolve::SceneResolver;

/// Where an [`AssetLibrary`]'s bytes come from.
enum AssetStore {
    /// A project's `assets/` folder, read straight off disk.
    Directory(PathBuf),
    /// A packed `assets.pak`, as an exported build ships.
    Bundle(Box<Bundle>),
}

/// Every asset in a project, indexed by the stable id in its `.meta`
/// sidecar.
///
/// Built once at load and then read-only: a game asks for an id and gets
/// bytes. Sidecars are only *read* here, never created — a shipped game
/// must not write into its own installation directory just by starting.
///
/// ## Loose files and packed bundles
///
/// A directory can serve a model *by path*, so a `.gltf` that references
/// `../textures/grass.png` resolves. A bundle cannot — there are no
/// paths inside it — so a packed export needs models that carry their
/// textures. [`AssetLibrary::import_mesh`] picks the right importer for
/// whichever store it has.
pub struct AssetLibrary {
    store: AssetStore,
    /// Asset id to store-relative path.
    by_id: HashMap<String, String>,
}

impl AssetLibrary {
    /// Walks `assets_dir` and indexes every asset that has a `.meta`
    /// sidecar.
    ///
    /// A file with no sidecar is skipped rather than rejected: it is
    /// simply not referenceable by id yet (nothing has imported it), and
    /// a stray `notes.txt` in an assets folder must not stop a game from
    /// starting. A malformed sidecar is logged and skipped for the same
    /// reason.
    ///
    /// # Errors
    ///
    /// [`SceneError::Io`] if `assets_dir` cannot be read at all.
    pub fn index(assets_dir: &Path) -> Result<Self, SceneError> {
        if !assets_dir.is_dir() {
            return Err(SceneError::Io(format!(
                "asset directory not found: {}",
                assets_dir.display()
            )));
        }

        let mut by_id = HashMap::new();
        let mut stack = vec![assets_dir.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let entries = std::fs::read_dir(&dir)
                .map_err(|err| SceneError::Io(format!("{}: {err}", dir.display())))?;
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                // Sidecars name their asset; assets are found through
                // them, so a file without one is simply not indexed.
                if path
                    .extension()
                    .is_none_or(|ext| ext != AssetMeta::EXTENSION)
                {
                    continue;
                }
                let asset_path = path.with_extension("");
                let Ok(relative) = asset_path.strip_prefix(assets_dir) else {
                    continue;
                };
                match read_meta_id(&path) {
                    Some(id) => {
                        by_id.insert(id, relative.to_string_lossy().replace('\\', "/"));
                    }
                    None => tracing::warn!(
                        path = %path.display(),
                        "skipping unreadable .meta sidecar"
                    ),
                }
            }
        }

        tracing::info!(
            dir = %assets_dir.display(),
            assets = by_id.len(),
            "indexed project assets"
        );
        Ok(Self {
            store: AssetStore::Directory(assets_dir.to_path_buf()),
            by_id,
        })
    }

    /// Indexes a packed bundle, as a shipped export carries.
    ///
    /// # Errors
    ///
    /// Never currently; returns `Result` so a future bundle format that
    /// can fail to enumerate does not change this signature.
    pub fn from_bundle(bundle: Bundle) -> Result<Self, SceneError> {
        let mut by_id = HashMap::new();
        let paths: Vec<String> = bundle.paths().map(str::to_string).collect();
        for meta_path in paths {
            let Some(asset_path) = meta_path.strip_suffix(".meta") else {
                continue;
            };
            let Some(bytes) = bundle.get(&meta_path) else {
                continue;
            };
            let Ok(text) = std::str::from_utf8(bytes) else {
                continue;
            };
            if let Ok(meta) = ron::from_str::<AssetMeta>(text) {
                by_id.insert(meta.id.to_string(), asset_path.to_string());
            }
        }
        tracing::info!(assets = by_id.len(), "indexed bundled assets");
        Ok(Self {
            store: AssetStore::Bundle(Box::new(bundle)),
            by_id,
        })
    }

    /// The file backing `id`, if this library reads loose files and
    /// carries that asset. Always `None` for a packed bundle.
    pub fn path_for(&self, id: &str) -> Option<PathBuf> {
        let relative = self.by_id.get(id)?;
        match &self.store {
            AssetStore::Directory(root) => Some(root.join(relative)),
            AssetStore::Bundle(_) => None,
        }
    }

    /// The id of the asset at `relative` (a `/`-separated path under
    /// the assets root), if this library carries it.
    ///
    /// The reverse of [`AssetLibrary::path_for`], for the case a game
    /// knows a well-known asset by name — its character model, say —
    /// rather than through a scene reference.
    /// Reads one asset by its store-relative path, whichever store this
    /// library has.
    ///
    /// The id-based [`AssetLibrary::read`] is for what a scene
    /// references; this is for what a game names itself — a footstep
    /// sound the code plays directly, say. Both have to work in a packed
    /// export, where there is no directory to open a file from.
    ///
    /// `Ok(None)` means the store has no such path.
    ///
    /// # Errors
    ///
    /// [`SceneError::Io`] if the path exists but cannot be read.
    pub fn read_path(&self, relative: &str) -> Result<Option<Vec<u8>>, SceneError> {
        match &self.store {
            AssetStore::Directory(root) => {
                let path = root.join(relative);
                if !path.is_file() {
                    return Ok(None);
                }
                std::fs::read(&path)
                    .map(Some)
                    .map_err(|err| SceneError::Io(format!("{}: {err}", path.display())))
            }
            AssetStore::Bundle(bundle) => Ok(bundle.get(relative).map(<[u8]>::to_vec)),
        }
    }

    /// The id recorded for a store-relative path, if the library knows
    /// it — the reverse of [`AssetLibrary::path_for`], for game code
    /// that names an asset by where it lives rather than by id.
    pub fn id_for_path(&self, relative: &str) -> Option<&str> {
        self.by_id
            .iter()
            .find(|(_, path)| path.as_str() == relative)
            .map(|(id, _)| id.as_str())
    }

    /// Reads the bytes of the asset `id` names.
    ///
    /// # Errors
    ///
    /// [`SceneError::Io`] if the asset is indexed but its bytes cannot
    /// be read — an inconsistent project, worth reporting rather than
    /// treating as "absent".
    pub fn read(&self, id: &str) -> Result<Option<Vec<u8>>, SceneError> {
        let Some(relative) = self.by_id.get(id) else {
            return Ok(None);
        };
        match &self.store {
            AssetStore::Directory(root) => {
                let path = root.join(relative);
                std::fs::read(&path)
                    .map(Some)
                    .map_err(|err| SceneError::Io(format!("{}: {err}", path.display())))
            }
            AssetStore::Bundle(bundle) => bundle.get(relative).map(<[u8]>::to_vec).map_or_else(
                || {
                    Err(SceneError::Io(format!(
                        "{relative} is indexed but missing from the bundle"
                    )))
                },
                |bytes| Ok(Some(bytes)),
            ),
        }
    }

    /// Imports the mesh `id` names, by path when this library has one so
    /// external texture references resolve, and from bytes otherwise.
    ///
    /// # Errors
    ///
    /// [`SceneError::Resolve`] if the file is not importable glTF.
    pub fn import_mesh(&self, id: &str) -> Result<Option<ImportedGltf>, SceneError> {
        if let Some(path) = self.path_for(id) {
            return import_gltf_file(&path)
                .map(Some)
                .map_err(|err| SceneError::Resolve(format!("{}: {err}", path.display())));
        }
        let Some(bytes) = self.read(id)? else {
            return Ok(None);
        };
        // In a bundle the model's textures are neighbouring keys, so the
        // URIs it wrote (`../textures/grass.png`) are resolved against
        // its own key rather than refused. Without this a packed export
        // loads every model as an untextured failure — which is exactly
        // what shipping one did.
        let relative = self.by_id.get(id).cloned().unwrap_or_default();
        let base = relative.rsplit_once('/').map_or("", |(dir, _)| dir);
        let resolve = |uri: &str| -> Option<Vec<u8>> {
            let key = join_relative(base, uri)?;
            self.read_path(&key).ok().flatten()
        };
        import_gltf_slice_with(&bytes, &resolve)
            .map(Some)
            .map_err(|err| SceneError::Resolve(format!("asset {id}: {err}")))
    }

    /// How many assets are indexed.
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    /// Whether the project carries no indexed assets.
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }
}

/// Joins a glTF's own URI onto the directory it lives in, resolving
/// `.` and `..` — bundle keys are plain strings with no filesystem to
/// normalize them.
///
/// `None` if the path climbs above the store's root: a model must not
/// be able to name its way out of the bundle it is packed in.
fn join_relative(base: &str, uri: &str) -> Option<String> {
    let mut parts: Vec<&str> = if base.is_empty() {
        Vec::new()
    } else {
        base.split('/').collect()
    };
    for component in uri.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    Some(parts.join("/"))
}

/// Reads just the `id` out of a `.meta` sidecar.
fn read_meta_id(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let meta: AssetMeta = ron::from_str(&text).ok()?;
    Some(meta.id.to_string())
}

/// Resolves a scene's asset references against an [`AssetLibrary`],
/// uploading what it imports.
///
/// The runtime twin of the editor's resolver: both decode with
/// `engine_asset` and upload through [`MeshRenderer::from_geometry`], so
/// a model looks the same in the editor and in the shipped game. Only
/// where the bytes come from differs.
pub struct RuntimeResolver<'a> {
    library: &'a AssetLibrary,
    gpu: &'a GpuContext,
    pipeline: &'a Pipeline,
    assets: &'a mut RenderAssets,
    /// Meshes already uploaded this load, so a scene placing one model
    /// many times uploads it once.
    uploaded: HashMap<AssetId, (AssetHandle<Mesh>, AssetHandle<MaterialBinding>)>,
}

impl<'a> RuntimeResolver<'a> {
    /// A resolver reading from `library` and uploading into `assets`.
    pub fn new(
        library: &'a AssetLibrary,
        gpu: &'a GpuContext,
        pipeline: &'a Pipeline,
        assets: &'a mut RenderAssets,
    ) -> Self {
        Self {
            library,
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

/// Converts an imported glTF material into a renderer material.
///
/// Without this a baked alpha-masked material loads as opaque, and
/// foliage cards render as solid rectangles — the exact bug the mask
/// exists to prevent.
pub fn material_from_imported(imported: &ImportedMaterial) -> Material {
    Material {
        base_color_factor: imported.base_color_factor,
        metallic_factor: imported.metallic_factor,
        roughness_factor: imported.roughness_factor,
        emissive_factor: imported.emissive_factor,
        normal_scale: imported.normal_scale,
        occlusion_strength: imported.occlusion_strength,
        alpha_mode: match imported.alpha_mode {
            "MASK" => AlphaMode::Mask {
                cutoff: imported.alpha_cutoff,
            },
            "BLEND" => AlphaMode::Blend,
            _ => AlphaMode::Opaque,
        },
    }
}

impl SceneResolver for RuntimeResolver<'_> {
    fn resolve_mesh(
        &mut self,
        mesh: &AssetRef,
        _material: &AssetRef,
    ) -> Result<Option<MeshRenderer>, SceneError> {
        let uuid = mesh.parse_id().map_err(SceneError::Resolve)?;
        let id = AssetId::from_uuid(uuid);

        // Already uploaded for an earlier entity. The ref-count check
        // guards a stale entry: if everything using this mesh was
        // despawned, the store freed it and the handle points at nothing.
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

        // Not carried by this project is not a failure: the scene may
        // reference something not shipped, and the entity is spawned
        // without geometry and counted as unresolved.
        let Some(gltf) = self.library.import_mesh(&mesh.id)? else {
            return Ok(None);
        };
        let Some(primitive) = gltf.meshes.first() else {
            return Err(SceneError::Resolve(format!(
                "asset {} contains no mesh primitives",
                mesh.id
            )));
        };
        let base_color = gltf
            .images
            .first()
            .map(|image| (image.width, image.height, image.rgba8.as_slice()));
        let material = gltf
            .materials
            .first()
            .map_or(Material::DEFAULT, material_from_imported);

        let renderer = MeshRenderer::from_geometry(
            self.gpu,
            self.pipeline,
            self.assets,
            &primitive.vertices,
            &primitive.indices,
            base_color,
            material,
        )
        .map_err(|err| SceneError::Resolve(err.to_string()))?;

        self.uploaded.insert(id, (renderer.mesh, renderer.material));
        Ok(Some(renderer))
    }

    fn resolve_sprite_uv(
        &mut self,
        atlas: &AssetRef,
        _region: &str,
    ) -> Result<Option<UvRect>, SceneError> {
        let Some(bytes) = self.library.read(&atlas.id)? else {
            return Ok(None);
        };
        // Decoded to prove the atlas is a real image; named regions need
        // an atlas layout nothing writes yet, so a sprite samples the
        // whole texture.
        import_texture_bytes(&bytes)
            .map_err(|err| SceneError::Resolve(format!("atlas {}: {err}", atlas.id)))?;
        Ok(Some(UvRect {
            min: [0.0, 0.0],
            max: [1.0, 1.0],
        }))
    }

    fn resolve_sound(
        &mut self,
        sound: &AssetRef,
        looping: bool,
    ) -> Result<Option<engine_audio::StaticSound>, SceneError> {
        // Not decoded until something asks: an emitter's clip is loaded
        // once here and then reference-counted by kira, so two entities
        // naming the same footstep do not decode it twice.
        let Some(bytes) = self.library.read(&sound.id)? else {
            return Ok(None);
        };
        engine_audio::StaticSound::from_bytes(bytes, looping)
            .map(Some)
            .map_err(|err| SceneError::Resolve(format!("sound {}: {err}", sound.id)))
    }
}

/// A mesh's geometry on the CPU: vertices and triangle-list indices.
pub type MeshGeometry = (Vec<engine_renderer::Vertex>, Vec<u32>);

/// Imports the mesh `reference` names, for a caller that needs its
/// geometry on the CPU rather than on the GPU — building a collision
/// mesh from the model that is actually drawn, most of all.
///
/// # Errors
///
/// [`SceneError::Resolve`] if the file cannot be imported or holds no
/// mesh. `Ok(None)` if the project does not carry that asset.
pub fn load_mesh_geometry(
    library: &AssetLibrary,
    reference: &AssetRef,
) -> Result<Option<MeshGeometry>, SceneError> {
    let Some(gltf) = library.import_mesh(&reference.id)? else {
        return Ok(None);
    };
    let Some(primitive) = gltf.meshes.first() else {
        return Err(SceneError::Resolve(format!(
            "asset {} contains no mesh primitives",
            reference.id
        )));
    };
    Ok(Some((
        primitive.vertices.clone(),
        primitive.indices.clone(),
    )))
}

/// A skinned model loaded from a project: its vertices, indices,
/// skeleton and clips, ready to hand to `SkinnedMeshRenderer` and
/// `AnimationPlayer`.
pub struct LoadedSkinnedMesh {
    /// Vertices with their joint bindings, in the form
    /// `GpuContext::create_skinned_mesh` expects.
    pub vertices: Vec<engine_renderer::SkinnedVertex>,
    /// Triangle-list indices.
    pub indices: Vec<u32>,
    /// The skin the vertices are bound to.
    pub skeleton: std::sync::Arc<engine_asset::ImportedSkeleton>,
    /// Every clip in the file, in file order.
    pub animations: Vec<std::sync::Arc<engine_asset::ImportedAnimation>>,
}

impl LoadedSkinnedMesh {
    /// The clip named `name`, if the file carries one.
    pub fn animation(
        &self,
        name: &str,
    ) -> Option<&std::sync::Arc<engine_asset::ImportedAnimation>> {
        self.animations
            .iter()
            .find(|clip| clip.name.as_deref() == Some(name))
    }
}

/// Loads the skinned mesh `reference` names — geometry, skin weights,
/// skeleton and clips.
///
/// The importer returns positions and joint bindings in two parallel
/// lists, because that is glTF's own layout; the renderer wants them
/// interleaved. Zipping them is the kind of chore every game loading a
/// character would otherwise write for itself.
///
/// # Errors
///
/// [`SceneError::Resolve`] if the file has no mesh, or its first mesh
/// carries no skin weights (it is not a skinned model). `Ok(None)` if
/// the project does not carry that asset.
pub fn load_skinned_mesh(
    library: &AssetLibrary,
    reference: &AssetRef,
) -> Result<Option<LoadedSkinnedMesh>, SceneError> {
    let Some(gltf) = library.import_mesh(&reference.id)? else {
        return Ok(None);
    };
    let Some(mesh) = gltf.meshes.first() else {
        return Err(SceneError::Resolve(format!(
            "asset {} contains no mesh primitives",
            reference.id
        )));
    };
    let Some(weights) = &mesh.skin_weights else {
        return Err(SceneError::Resolve(format!(
            "asset {} has no skin weights; it is not a skinned mesh",
            reference.id
        )));
    };
    let skeleton_index = mesh.skeleton.ok_or_else(|| {
        SceneError::Resolve(format!(
            "asset {} has skin weights but no skeleton to bind them to",
            reference.id
        ))
    })?;
    let skeleton = gltf.skeletons.get(skeleton_index).ok_or_else(|| {
        SceneError::Resolve(format!(
            "asset {} names skeleton {skeleton_index}, which the file does not have",
            reference.id
        ))
    })?;

    let vertices = mesh
        .vertices
        .iter()
        .zip(weights.joints.iter().zip(&weights.weights))
        .map(
            |(vertex, (joints, weights))| engine_renderer::SkinnedVertex {
                position: vertex.position,
                normal: vertex.normal,
                uv: vertex.uv,
                joints: joints.map(u32::from),
                weights: *weights,
            },
        )
        .collect();

    Ok(Some(LoadedSkinnedMesh {
        vertices,
        indices: mesh.indices.clone(),
        skeleton: std::sync::Arc::new(skeleton.clone()),
        animations: gltf
            .animations
            .iter()
            .cloned()
            .map(std::sync::Arc::new)
            .collect(),
    }))
}

/// The packed asset bundle's file name in an exported game folder.
///
/// Shared with `engine_project`'s exporter so the name a build writes
/// and the name a game looks for cannot drift apart.
pub const BUNDLE_FILE: &str = "assets.pak";

/// The scene file's name in an exported game folder.
pub const EXPORT_SCENE_FILE: &str = "main.ron";

/// An exported game's content: the packed assets and the scene that
/// starts, both read from beside the executable.
pub struct ExportContent {
    /// Every asset in the export, indexed by id.
    pub library: AssetLibrary,
    /// The scene to instantiate.
    pub scene: Scene,
}

/// Opens the exported game in `dir`, if `dir` holds one.
///
/// `None` means there is no [`BUNDLE_FILE`] there — the caller is
/// running from a project directory during development, not from a
/// shipped folder, and should load the project instead. `Some(Err(..))`
/// means there *is* an export there and it is broken, which is worth
/// reporting rather than silently falling back to a project that may not
/// exist on a player's machine.
///
/// # Errors
///
/// [`SceneError::Io`] if the bundle or the scene cannot be read, or
/// [`SceneError`] from parsing the scene.
pub fn open_export(dir: &Path) -> Option<Result<ExportContent, SceneError>> {
    let bundle_path = dir.join(BUNDLE_FILE);
    if !bundle_path.is_file() {
        return None;
    }
    Some(load_export(dir, &bundle_path))
}

fn load_export(dir: &Path, bundle_path: &Path) -> Result<ExportContent, SceneError> {
    let bundle = Bundle::open(bundle_path)
        .map_err(|err| SceneError::Io(format!("{}: {err}", bundle_path.display())))?;
    let files = bundle.len();
    let library = AssetLibrary::from_bundle(bundle)?;
    let scene_path = dir.join(EXPORT_SCENE_FILE);
    let scene = Scene::load_from_file(&scene_path)?;
    tracing::info!(
        dir = %dir.display(),
        bundle_files = files,
        entities = scene.entities.len(),
        "opened an exported game folder"
    );
    Ok(ExportContent { library, scene })
}

/// The directory the running executable lives in, if it can be found.
///
/// This is where a shipped game's content sits — not the working
/// directory, which is wherever the player happened to double-click
/// from.
pub fn executable_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.parent().map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A project-shaped temp directory with one asset and its sidecar.
    fn temp_project(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("vge-asset-library-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("models")).expect("temp dirs");
        root
    }

    #[test]
    fn a_bundle_beside_the_executable_is_preferred() {
        let root = temp_project("export-folder");
        // A project-shaped assets directory, packed the way `export`
        // packs one.
        write_asset(
            &root,
            "models/rock.gltf",
            "11111111-1111-1111-1111-111111111111",
        );
        let bundle_path = root.join(BUNDLE_FILE);
        engine_asset::pack_dir(&root, &bundle_path).expect("pack");
        std::fs::write(root.join(EXPORT_SCENE_FILE), "(version: 3, entities: [])")
            .expect("write scene");

        let content = open_export(&root)
            .expect("a folder with a bundle is an export")
            .expect("and it opens");
        assert_eq!(
            content.library.id_for_path("models/rock.gltf"),
            Some("11111111-1111-1111-1111-111111111111"),
            "ids resolve out of the packed sidecars"
        );
        assert_eq!(content.library.len(), 1);
        // A bundle has no filesystem paths, so this is deliberately
        // `None` — everything must go through the library.
        assert_eq!(
            content
                .library
                .path_for("11111111-1111-1111-1111-111111111111"),
            None
        );
        assert!(content.scene.entities.is_empty());

        // The same asset is readable by path with no directory to open.
        assert!(
            content
                .library
                .read_path("models/rock.gltf")
                .expect("read")
                .is_some()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_uri_is_joined_onto_the_models_own_key() {
        assert_eq!(
            join_relative("models", "../textures/grass.png").as_deref(),
            Some("textures/grass.png")
        );
        assert_eq!(
            join_relative("models/props", "./rock.bin").as_deref(),
            Some("models/props/rock.bin")
        );
        assert_eq!(
            join_relative("", "terrain.gltf").as_deref(),
            Some("terrain.gltf")
        );
        // A model cannot name its way out of the bundle.
        assert_eq!(join_relative("models", "../../etc/passwd"), None);
    }

    #[test]
    fn a_folder_without_a_bundle_is_not_an_export() {
        let root = temp_project("not-an-export");
        write_asset(
            &root,
            "models/rock.gltf",
            "22222222-2222-2222-2222-222222222222",
        );
        assert!(
            open_export(&root).is_none(),
            "no assets.pak means run from the project instead"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_broken_export_reports_rather_than_falling_back() {
        let root = temp_project("broken-export");
        std::fs::write(root.join(BUNDLE_FILE), b"not a bundle").expect("write");
        // A player's machine has no project to fall back to, so a
        // corrupt export has to be an error, not a shrug.
        assert!(matches!(open_export(&root), Some(Err(_))));
        let _ = std::fs::remove_dir_all(&root);
    }

    fn write_asset(dir: &Path, name: &str, id: &str) {
        let path = dir.join(name);
        std::fs::write(&path, b"not really a model").expect("write asset");
        std::fs::write(
            AssetMeta::sidecar_path(&path),
            format!("(id: \"{id}\", source_hash: 0)"),
        )
        .expect("write sidecar");
    }

    #[test]
    fn index_maps_ids_to_files_recursively() {
        let root = temp_project("index");
        write_asset(
            &root.join("models"),
            "tree.gltf",
            "3fa85f64-5717-4562-b3fc-2c963f66afa6",
        );

        let library = AssetLibrary::index(&root).expect("index");
        assert_eq!(library.len(), 1);
        let path = library
            .path_for("3fa85f64-5717-4562-b3fc-2c963f66afa6")
            .expect("indexed");
        assert!(path.ends_with("models/tree.gltf"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_asset_directory_is_a_clean_error() {
        let missing = std::env::temp_dir().join("vge-asset-library-does-not-exist");
        let _ = std::fs::remove_dir_all(&missing);
        assert!(matches!(
            AssetLibrary::index(&missing),
            Err(SceneError::Io(_))
        ));
    }

    #[test]
    fn files_without_a_sidecar_are_skipped_not_fatal() {
        let root = temp_project("no-sidecar");
        std::fs::write(root.join("models/loose.gltf"), b"x").expect("write");
        std::fs::write(root.join("README.txt"), b"hello").expect("write");

        let library = AssetLibrary::index(&root).expect("index");
        assert!(library.is_empty(), "nothing is referenceable by id yet");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_malformed_sidecar_is_skipped_not_fatal() {
        let root = temp_project("bad-sidecar");
        let path = root.join("models/tree.gltf");
        std::fs::write(&path, b"x").expect("write");
        std::fs::write(AssetMeta::sidecar_path(&path), b"{ this is not ron").expect("write");

        let library = AssetLibrary::index(&root).expect("one bad file must not stop a game");
        assert!(library.is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reading_an_unknown_id_is_none_not_an_error() {
        let root = temp_project("unknown");
        let library = AssetLibrary::index(&root).expect("index");
        assert!(
            library
                .read("3fa85f64-5717-4562-b3fc-2c963f66afa6")
                .expect("no error")
                .is_none()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn alpha_mask_survives_the_material_conversion() {
        // A canopy baked as MASK must not load as opaque, or every
        // foliage card renders as a solid rectangle.
        let imported = ImportedMaterial {
            name: Some("leaf".to_string()),
            base_color_factor: [1.0, 1.0, 1.0, 1.0],
            metallic_factor: 0.0,
            roughness_factor: 0.9,
            base_color_image: None,
            emissive_factor: [0.0; 3],
            emissive_image: None,
            normal_image: None,
            normal_scale: 1.0,
            occlusion_image: None,
            occlusion_strength: 1.0,
            alpha_mode: "MASK",
            alpha_cutoff: 0.4,
        };
        let material = material_from_imported(&imported);
        assert!(matches!(
            material.alpha_mode,
            AlphaMode::Mask { cutoff } if (cutoff - 0.4).abs() < 1e-6
        ));
        assert_eq!(material.roughness_factor, 0.9);
    }
}
