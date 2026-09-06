//! Standalone RustyEngine runtime.
//!
//! Runs an *exported* game folder: a `main.ron` scene plus an
//! `assets.pak` bundle (or a loose `assets/` directory as a fallback).
//! Nothing here links the editor or AI crates — `engine` is depended on
//! with `default-features = false` so the `studio` feature is off
//! (NFR-004).
//!
//! Usage: `player [<export-dir>]` (defaults to the current directory).
//!
//! ## Shape
//!
//! A thin [`Game`] implementation, nothing more. [`run_game`] owns the
//! window, GPU, render pipelines, world and frame loop; [`Player::setup`]
//! loads the bundle, parses the scene, and resolves its asset references
//! into live renderable components through [`BundleResolver`]. Deliberate:
//! the player exercises the same public API a hand-written game uses, so
//! anything missing from it shows up here first.
//!
//! ## Failure stance
//!
//! An export is untrusted input. A missing or corrupt bundle is a clean
//! error and a non-zero exit; a single broken asset reference inside an
//! otherwise-good scene is a warning and a skipped entity, so one bad
//! file never costs you the rest of the level.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use engine::asset::{AssetId, Bundle, ImportedGltf, import_gltf_slice, import_texture_bytes};
use engine::ecs::components::MeshRenderer;
use engine::prelude::{
    AssetRef, Game, GameConfig, GameContext, GameError, Material, Scene, SceneError, SceneResolver,
    UvRect, run_game,
};
use engine::renderer::{GpuContext, Pipeline, RenderAssets};

/// Where a game's assets come from at runtime: a packed `.pak` or a
/// loose directory.
enum AssetSource {
    /// A parsed `assets.pak`.
    Bundle(Box<Bundle>),
    /// A `<dir>/assets/` tree.
    Loose(PathBuf),
}

impl AssetSource {
    /// Resolves the asset source for an export directory: prefer
    /// `assets.pak`, fall back to a loose `assets/` dir, else `None`.
    fn resolve(export_dir: &Path) -> Option<AssetSource> {
        let pak = export_dir.join("assets.pak");
        if pak.is_file() {
            match Bundle::open(&pak) {
                Ok(bundle) => {
                    tracing::info!(files = bundle.len(), "opened asset bundle");
                    return Some(AssetSource::Bundle(Box::new(bundle)));
                }
                Err(err) => {
                    tracing::warn!(error = %err, "assets.pak is unreadable; trying loose files")
                }
            }
        }
        let loose = export_dir.join("assets");
        if loose.is_dir() {
            tracing::info!(dir = %loose.display(), "using loose asset directory");
            return Some(AssetSource::Loose(loose));
        }
        None
    }

    /// Reads one asset by its bundle-relative path.
    fn read(&self, relative: &str) -> Option<Vec<u8>> {
        match self {
            AssetSource::Bundle(bundle) => bundle.get(relative).map(<[u8]>::to_vec),
            AssetSource::Loose(dir) => std::fs::read(dir.join(relative)).ok(),
        }
    }

    /// Every asset path this source can serve, so ids can be indexed up
    /// front from the `.meta` sidecars.
    fn paths(&self) -> Vec<String> {
        match self {
            AssetSource::Bundle(bundle) => bundle.paths().map(str::to_string).collect(),
            AssetSource::Loose(dir) => {
                let mut out = Vec::new();
                collect_paths(dir, dir, &mut out);
                out
            }
        }
    }
}

/// Recursively collects file paths under `dir`, relative to `root`, in
/// the same `/`-separated form a bundle uses.
fn collect_paths(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_paths(root, &path, out);
        } else if path.is_file()
            && let Ok(relative) = path.strip_prefix(root)
        {
            out.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
}

/// Reads the `id` field out of a `.meta` sidecar's RON text.
///
/// A deliberately small parser rather than pulling in the editor's meta
/// types: the sidecar's shape is `(id: "<uuid>", source_hash: <n>)`, and
/// the id is all the runtime needs.
fn parse_meta_id(text: &str) -> Option<String> {
    let start = text.find("id:")? + 3;
    let rest = &text[start..];
    let open = rest.find('"')? + 1;
    let close = rest[open..].find('"')? + open;
    Some(rest[open..close].to_string())
}

/// Resolves an exported scene's asset references against the bundle.
///
/// The runtime twin of the editor's resolver. Both decode with
/// `engine_asset` and upload through
/// [`MeshRenderer::from_geometry`], so the two agree on what a given
/// asset looks like; only where the *bytes* come from differs.
struct BundleResolver<'a> {
    source: &'a AssetSource,
    /// Asset id to bundle-relative path, from the `.meta` sidecars.
    paths_by_id: &'a HashMap<String, String>,
    gpu: &'a GpuContext,
    pipeline: &'a Pipeline,
    assets: &'a mut RenderAssets,
    /// Meshes already uploaded, so a scene placing one model many times
    /// pays for one upload.
    uploaded: HashMap<
        AssetId,
        (
            engine::utils::AssetHandle<engine::renderer::Mesh>,
            engine::utils::AssetHandle<engine::renderer::MaterialBinding>,
        ),
    >,
}

/// Decodes the glTF stored under `reference`'s id.
///
/// `Ok(None)` means the export simply doesn't carry that asset — normal
/// for a scene referencing something that was never packed, and handled
/// upstream as "spawn the entity without geometry". An indexed asset
/// whose bytes are missing is a genuine error: the export is inconsistent.
///
/// A free function rather than a method so it can be tested without a
/// GPU — everything below it needs a live device.
fn load_gltf(
    source: &AssetSource,
    paths_by_id: &HashMap<String, String>,
    reference: &AssetRef,
) -> Result<Option<ImportedGltf>, SceneError> {
    let Some(path) = paths_by_id.get(&reference.id) else {
        return Ok(None);
    };
    let Some(bytes) = source.read(path) else {
        return Err(SceneError::Resolve(format!(
            "asset {path} is indexed but its bytes are missing from the export"
        )));
    };
    import_gltf_slice(&bytes)
        .map(Some)
        .map_err(|err| SceneError::Resolve(format!("{path}: {err}")))
}

impl SceneResolver for BundleResolver<'_> {
    fn resolve_mesh(
        &mut self,
        mesh: &AssetRef,
        _material: &AssetRef,
    ) -> Result<Option<MeshRenderer>, SceneError> {
        let uuid = mesh.parse_id().map_err(SceneError::Resolve)?;
        let id = AssetId::from_uuid(uuid);

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

        let Some(gltf) = load_gltf(self.source, self.paths_by_id, mesh)? else {
            return Ok(None);
        };
        let Some(primitive) = gltf.meshes.first() else {
            return Err(SceneError::Resolve(format!(
                "glTF asset {id} contains no mesh primitives"
            )));
        };

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

        self.uploaded.insert(id, (renderer.mesh, renderer.material));
        Ok(Some(renderer))
    }

    fn resolve_sprite_uv(
        &mut self,
        atlas: &AssetRef,
        _region: &str,
    ) -> Result<Option<UvRect>, SceneError> {
        let Some(path) = self.paths_by_id.get(&atlas.id) else {
            return Ok(None);
        };
        let Some(bytes) = self.source.read(path) else {
            return Ok(None);
        };
        // Decoded purely to validate that the atlas is a real image;
        // named regions need an atlas layout the exporter doesn't write
        // yet, so every sprite samples the whole texture for now.
        import_texture_bytes(&bytes)
            .map_err(|err| SceneError::Resolve(format!("{path}: {err}")))?;
        Ok(Some(UvRect {
            min: [0.0, 0.0],
            max: [1.0, 1.0],
        }))
    }
}

/// Errors that stop the player before a window ever opens.
#[derive(Debug, thiserror::Error)]
enum PlayerError {
    /// No `main.ron` in the export directory or its bundle.
    #[error("no main.ron scene found in {0}")]
    MissingScene(PathBuf),
    /// `main.ron` exists but isn't a usable scene.
    #[error("scene could not be loaded: {0}")]
    Scene(#[from] SceneError),
    /// The scene text wasn't valid UTF-8.
    #[error("main.ron is not valid UTF-8 text")]
    SceneEncoding,
}

/// Reads and parses the export's scene. A loose `main.ron` wins over a
/// bundled one, for quick iteration on an export.
fn load_scene(export_dir: &Path, assets: Option<&AssetSource>) -> Result<Scene, PlayerError> {
    let loose = export_dir.join("main.ron");
    let bytes = match std::fs::read(&loose) {
        Ok(bytes) => bytes,
        Err(_) => assets
            .and_then(|source| source.read("main.ron"))
            .ok_or_else(|| PlayerError::MissingScene(export_dir.to_path_buf()))?,
    };
    let text = String::from_utf8(bytes).map_err(|_| PlayerError::SceneEncoding)?;
    let scene = Scene::from_ron_str(&text)?;
    scene.validate()?;
    Ok(scene)
}

/// Builds the id-to-path index from every `.meta` sidecar the asset
/// source carries.
fn index_asset_ids(source: &AssetSource) -> HashMap<String, String> {
    let mut index = HashMap::new();
    for meta_path in source.paths() {
        let Some(asset_path) = meta_path.strip_suffix(".meta") else {
            continue;
        };
        let Some(bytes) = source.read(&meta_path) else {
            continue;
        };
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        if let Some(id) = parse_meta_id(&text) {
            index.insert(id, asset_path.to_string());
        }
    }
    index
}

/// The exported game, as a [`Game`].
struct Player {
    scene: Scene,
    assets: Option<AssetSource>,
    paths_by_id: HashMap<String, String>,
}

impl Game for Player {
    fn setup(&mut self, ctx: &mut GameContext<'_>) -> Result<(), GameError> {
        // Adopt the scene's own camera, if it defines one, before
        // anything else looks at the view.
        if let Some(camera) = self
            .scene
            .entities
            .iter()
            .find_map(|entity| entity.camera.as_ref())
        {
            *ctx.camera_mut() = engine::renderer::Camera::from(*camera);
        }

        let parts = ctx.scene_parts();
        let report = match &self.assets {
            Some(source) => {
                let mut resolver = BundleResolver {
                    source,
                    paths_by_id: &self.paths_by_id,
                    gpu: parts.gpu,
                    pipeline: parts.pipeline,
                    assets: parts.assets,
                    uploaded: HashMap::new(),
                };
                self.scene
                    .instantiate_with_resolver(parts.world, &mut resolver)
            }
            None => {
                tracing::warn!("no asset source; entities will spawn without geometry");
                self.scene
                    .instantiate_with_resolver(parts.world, &mut engine::scene::NullResolver)
            }
        };

        tracing::info!(
            entities = report.spawned.len(),
            unresolved = report.unresolved,
            "scene instantiated"
        );
        if report.unresolved > 0 {
            tracing::warn!(
                unresolved = report.unresolved,
                "some entities are missing their geometry; the rest of the scene still runs"
            );
        }
        Ok(())
    }

    fn update(&mut self, ctx: &mut GameContext<'_>, _dt: f32) {
        if ctx.input().is_key_pressed(engine::prelude::KeyCode::Escape) {
            ctx.request_exit();
        }
    }
}

/// Loads the export and runs it.
fn run(export_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let assets = AssetSource::resolve(export_dir);
    if assets.is_none() {
        tracing::warn!("no assets.pak or assets/ directory found");
    }
    let paths_by_id = assets.as_ref().map(index_asset_ids).unwrap_or_default();
    tracing::info!(assets = paths_by_id.len(), "asset index ready");

    let scene = load_scene(export_dir, assets.as_ref())?;

    let config = GameConfig::new("RustyEngine Player", 1280, 720);
    run_game(
        config,
        Player {
            scene,
            assets,
            paths_by_id,
        },
    )?;
    Ok(())
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let export_dir = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    tracing::info!(dir = %export_dir.display(), "loading export");

    if let Err(err) = run(&export_dir) {
        tracing::error!(error = %err, "player exited with an error");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_id_is_parsed_from_a_sidecar() {
        let text = "(\n    id: \"b8d5d33b-086e-4494-b2bd-0cb556828369\",\n    source_hash: 42,\n)";
        assert_eq!(
            parse_meta_id(text).as_deref(),
            Some("b8d5d33b-086e-4494-b2bd-0cb556828369")
        );
    }

    #[test]
    fn malformed_meta_yields_no_id_rather_than_panicking() {
        assert!(parse_meta_id("").is_none());
        assert!(parse_meta_id("id:").is_none());
        assert!(parse_meta_id("id: \"unterminated").is_none());
        assert!(parse_meta_id("source_hash: 1").is_none());
    }

    #[test]
    fn missing_scene_is_a_clean_error() {
        let dir = std::env::temp_dir().join(format!("vge-player-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let result = load_scene(&dir, None);
        assert!(matches!(result, Err(PlayerError::MissingScene(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_scene_is_an_error_not_a_panic() {
        let dir = std::env::temp_dir().join(format!("vge-player-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(dir.join("main.ron"), "this is not RON at all {{{").expect("write");
        let result = load_scene(&dir, None);
        assert!(matches!(result, Err(PlayerError::Scene(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vge-player-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn loose_directory_is_used_when_there_is_no_pak() {
        let dir = temp_dir("loose");
        std::fs::create_dir_all(dir.join("assets")).expect("assets dir");
        std::fs::write(dir.join("assets/thing.bin"), b"data").expect("write");

        let source = AssetSource::resolve(&dir).expect("a loose dir is a valid source");
        assert_eq!(source.read("thing.bin").as_deref(), Some(&b"data"[..]));
        assert_eq!(source.paths(), vec!["thing.bin".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_pak_falls_back_to_loose_files_instead_of_failing() {
        let dir = temp_dir("corruptpak");
        std::fs::write(dir.join("assets.pak"), b"not a bundle at all").expect("write");
        std::fs::create_dir_all(dir.join("assets")).expect("assets dir");
        std::fs::write(dir.join("assets/thing.bin"), b"data").expect("write");

        let source = AssetSource::resolve(&dir).expect("must fall back, not give up");
        assert_eq!(source.read("thing.bin").as_deref(), Some(&b"data"[..]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_assets_at_all_is_none_not_a_panic() {
        let dir = temp_dir("noassets");
        assert!(AssetSource::resolve(&dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn asset_ids_are_indexed_from_meta_sidecars() {
        let dir = temp_dir("index");
        std::fs::create_dir_all(dir.join("assets")).expect("assets dir");
        std::fs::write(dir.join("assets/quad.gltf"), b"{}").expect("write");
        std::fs::write(
            dir.join("assets/quad.gltf.meta"),
            "(\n    id: \"b8d5d33b-086e-4494-b2bd-0cb556828369\",\n    source_hash: 1,\n)",
        )
        .expect("write");

        let source = AssetSource::resolve(&dir).expect("source");
        let index = index_asset_ids(&source);
        assert_eq!(
            index
                .get("b8d5d33b-086e-4494-b2bd-0cb556828369")
                .map(String::as_str),
            Some("quad.gltf"),
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unindexed_reference_is_absent_not_an_error() {
        let dir = temp_dir("unindexed");
        std::fs::create_dir_all(dir.join("assets")).expect("assets dir");
        let source = AssetSource::resolve(&dir).expect("source");

        let result = load_gltf(
            &source,
            &HashMap::new(),
            &AssetRef {
                id: "b8d5d33b-086e-4494-b2bd-0cb556828369".into(),
            },
        );
        assert!(matches!(result, Ok(None)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn indexed_asset_with_missing_bytes_is_an_error() {
        let dir = temp_dir("missingbytes");
        std::fs::create_dir_all(dir.join("assets")).expect("assets dir");
        let source = AssetSource::resolve(&dir).expect("source");

        let mut index = HashMap::new();
        index.insert("the-id".to_string(), "gone.gltf".to_string());
        let result = load_gltf(
            &source,
            &index,
            &AssetRef {
                id: "the-id".into(),
            },
        );
        assert!(
            matches!(result, Err(SceneError::Resolve(_))),
            "an inconsistent export must report, not silently render nothing"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn undecodable_asset_bytes_are_an_error_not_a_panic() {
        let dir = temp_dir("baddecode");
        std::fs::create_dir_all(dir.join("assets")).expect("assets dir");
        std::fs::write(dir.join("assets/broken.gltf"), b"definitely not glTF").expect("write");
        let source = AssetSource::resolve(&dir).expect("source");

        let mut index = HashMap::new();
        index.insert("the-id".to_string(), "broken.gltf".to_string());
        let result = load_gltf(
            &source,
            &index,
            &AssetRef {
                id: "the-id".into(),
            },
        );
        assert!(matches!(result, Err(SceneError::Resolve(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
