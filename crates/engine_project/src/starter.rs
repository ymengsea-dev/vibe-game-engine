//! Creation-only scenes for the New Project dialog.

use std::path::Path;

use engine_asset::{AssetMetadataStore, ExportMaterial, ExportMesh, encode_png_rgba8, export_gltf};
use engine_scene::{
    AssetRef, BodyKind, CameraData, ColliderData, ColliderShape, LightingData,
    MaterialOverrideData, MeshRendererData, ProjectionData, RuleAction, RuleData, RuleTrigger,
    Scene, SceneEntity, SpriteData, TransformData, UiElementData,
};

use crate::{GameplayLanguage, ProjectError, ProjectTemplate};

pub(super) fn create_scene(
    root: &Path,
    language: GameplayLanguage,
    template: ProjectTemplate,
) -> Result<Scene, ProjectError> {
    let mut scene = Scene::default();
    let (camera, title) = match template {
        ProjectTemplate::Blank => return Ok(scene),
        ProjectTemplate::Starter2D => {
            let path = root.join("assets/images/first_sprite.png");
            let mut pixels = vec![0_u8; 32 * 32 * 4];
            for y in 0..32 {
                for x in 0..32 {
                    let pixel = &mut pixels[(y * 32 + x) * 4..][..4];
                    pixel.copy_from_slice(if (5..27).contains(&x) && (5..27).contains(&y) {
                        &[89, 179, 236, 255]
                    } else {
                        &[0, 0, 0, 0]
                    });
                }
            }
            write_file(&path, &encode_png_rgba8(32, 32, &pixels)?)?;
            let asset = asset_ref(root, &path)?;
            scene.entities.push(SceneEntity {
                object_id: Some(uuid::Uuid::new_v4().to_string()),
                name: Some("First Sprite".into()),
                transform: Some(transform([0.0, 0.0, 0.0], [1.0; 3])),
                sprite: Some(SpriteData {
                    atlas: asset.clone(),
                    region: "full".into(),
                    size: [2.0, 2.0],
                    color: [1.0; 4],
                    z_order: 0.0,
                    animation: None,
                }),
                asset_source: Some("assets/images/first_sprite.png".into()),
                asset_id: Some(asset.id),
                ..Default::default()
            });
            (
                CameraData {
                    eye: [0.0, 0.0, 10.0],
                    target: [0.0; 3],
                    up: [0.0, 1.0, 0.0],
                    aspect_ratio: 16.0 / 9.0,
                    projection: ProjectionData::Orthographic { height: 6.0 },
                    near: 0.1,
                    far: 100.0,
                    pose_from_transform: false,
                    active: false,
                },
                "Your first 2D game",
            )
        }
        ProjectTemplate::Starter3D => {
            let path = root.join("assets/models/first_cube.gltf");
            let (vertices, indices) = engine_renderer::cube();
            let gltf = export_gltf(
                &[ExportMesh {
                    name: "Cube",
                    vertices: &vertices,
                    indices: &indices,
                    material: Some(0),
                }],
                &[ExportMaterial::new("Starter material")],
            )?;
            write_file(&path, &gltf)?;
            let asset = asset_ref(root, &path)?;
            for (name, position, scale, tint) in [
                (
                    "Ground",
                    [0.0, -0.65, 0.0],
                    [8.0, 0.2, 8.0],
                    [0.22, 0.36, 0.3],
                ),
                ("First Cube", [0.0, 0.0, 0.0], [1.0; 3], [0.35, 0.65, 0.95]),
            ] {
                scene.entities.push(SceneEntity {
                    object_id: Some(uuid::Uuid::new_v4().to_string()),
                    name: Some(name.into()),
                    transform: Some(transform(position, scale)),
                    mesh_renderer: Some(MeshRendererData {
                        mesh: asset.clone(),
                        material: asset.clone(),
                        material_override: Some(MaterialOverrideData {
                            tint,
                            ..Default::default()
                        }),
                    }),
                    collider: Some(ColliderData::new(
                        ColliderShape::Cuboid {
                            half_extents: [0.5; 3],
                        },
                        BodyKind::Fixed,
                    )),
                    asset_source: Some("assets/models/first_cube.gltf".into()),
                    asset_id: Some(asset.id.clone()),
                    ..Default::default()
                });
            }
            scene.lighting = Some(LightingData::default());
            (
                CameraData {
                    eye: [4.0, 3.0, 6.0],
                    target: [0.0, 0.0, 0.0],
                    up: [0.0, 1.0, 0.0],
                    aspect_ratio: 16.0 / 9.0,
                    projection: ProjectionData::Perspective { fov_y_radians: 1.0 },
                    near: 0.1,
                    far: 200.0,
                    pose_from_transform: false,
                    active: false,
                },
                "Your first 3D game",
            )
        }
    };
    scene.entities.push(SceneEntity {
        object_id: Some(uuid::Uuid::new_v4().to_string()),
        name: Some("Main Camera".into()),
        camera: Some(camera),
        ..Default::default()
    });
    let label_id = uuid::Uuid::new_v4().to_string();
    let mut label = UiElementData::label();
    label.text = title.into();
    label.size = [360.0, 32.0];
    scene.entities.push(SceneEntity {
        object_id: Some(label_id.clone()),
        name: Some("Welcome Label".into()),
        ui: Some(label),
        ..Default::default()
    });
    let mut button = UiElementData::button();
    button.text = "Click to begin".into();
    scene.entities.push(SceneEntity {
        object_id: Some(uuid::Uuid::new_v4().to_string()),
        name: Some("Start Button".into()),
        ui: Some(button),
        rules: vec![RuleData {
            trigger: RuleTrigger::UiClicked,
            action: RuleAction::SetUiText {
                target: label_id,
                text: "It works! Try editing this scene.".into(),
            },
            conditions: Vec::new(),
            match_mode: engine_scene::RuleMatch::All,
            graph_position: None,
        }],
        ..Default::default()
    });
    let code_path = if language == GameplayLanguage::Lua {
        "assets/scripts/main.lua"
    } else {
        "src/main.rs"
    };
    let guide = format!(
        "# {title}\n\nOpen `scenes/main.ron` in Studio, then press Play. Click the button to try an authored Visual Rule.\n\nSelect the sprite or cube in Hierarchy to change its position and appearance. Edit `{code_path}` to add gameplay, then press Play again.\n\nUse Project Assets to import your own images or models and drag them into Scene View. Save the scene before exporting.\n"
    );
    write_file(&root.join("START_HERE.md"), guide.as_bytes())?;
    Ok(scene)
}

fn transform(translation: [f32; 3], scale: [f32; 3]) -> TransformData {
    TransformData {
        translation,
        rotation: [0.0, 0.0, 0.0, 1.0],
        scale,
    }
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), ProjectError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| ProjectError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    std::fs::write(path, bytes).map_err(|source| ProjectError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn asset_ref(root: &Path, path: &Path) -> Result<AssetRef, ProjectError> {
    let store = AssetMetadataStore::new(root.join("assets"), root.join(".studio/assets"));
    Ok(AssetRef {
        id: store.load_or_create(path)?.id.to_string(),
    })
}
