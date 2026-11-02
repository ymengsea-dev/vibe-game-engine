//! Packs authored sprite images into the one texture used by the sprite pass.

use engine_asset::{ImportedTexture, import_texture_bytes};
use engine_ecs::components::{Sprite, SpriteSource};
use engine_ecs::prelude::World;
use engine_renderer::{AtlasLayout, PixelRect, UvRect};
use std::collections::BTreeSet;
use std::time::Duration;

use crate::{AssetLibrary, SceneError, SceneSpriteAnimation};

/// One cell in an evenly divided sprite sheet. `grid:` is reserved for
/// this format; older named regions continue to mean the full image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridRegion {
    /// Number of columns in the sheet.
    pub columns: u32,
    /// Number of rows in the sheet.
    pub rows: u32,
    /// Zero-based column to draw.
    pub column: u32,
    /// Zero-based row to draw.
    pub row: u32,
}

impl GridRegion {
    /// Reads `grid:columns,rows:column,row`; invalid or out-of-range cells fail.
    pub fn parse(text: &str) -> Option<Self> {
        let (grid, cell) = text.strip_prefix("grid:")?.split_once(':')?;
        let (columns, rows) = grid.split_once(',')?;
        let (column, row) = cell.split_once(',')?;
        let region = Self {
            columns: columns.parse().ok()?,
            rows: rows.parse().ok()?,
            column: column.parse().ok()?,
            row: row.parse().ok()?,
        };
        (region.columns > 0
            && region.rows > 0
            && region.column < region.columns
            && region.row < region.rows)
            .then_some(region)
    }

    /// Encodes a valid grid cell for a scene sprite's `region` field.
    pub fn name(self) -> String {
        format!(
            "grid:{},{}:{},{}",
            self.columns, self.rows, self.column, self.row
        )
    }

    fn uv(self, full: UvRect) -> UvRect {
        let width = full.max[0] - full.min[0];
        let height = full.max[1] - full.min[1];
        UvRect {
            min: [
                full.min[0] + width * self.column as f32 / self.columns as f32,
                full.min[1] + height * self.row as f32 / self.rows as f32,
            ],
            max: [
                full.min[0] + width * (self.column + 1) as f32 / self.columns as f32,
                full.min[1] + height * (self.row + 1) as f32 / self.rows as f32,
            ],
        }
    }
}

fn region_uv(full: UvRect, name: &str) -> Option<UvRect> {
    if name.starts_with("grid:") {
        Some(GridRegion::parse(name)?.uv(full))
    } else {
        Some(full)
    }
}

fn animation_uv(
    full: UvRect,
    source: &SpriteSource,
    animation: &SceneSpriteAnimation,
) -> Option<UvRect> {
    let grid = GridRegion::parse(&source.region)?;
    let first = u64::from(grid.row) * u64::from(grid.columns) + u64::from(grid.column);
    let frame = (animation.elapsed * animation.data.fps).floor() as u64;
    let index = first.checked_add(frame.min(u64::from(animation.data.frames.saturating_sub(1))))?;
    let columns = u64::from(grid.columns);
    (index < columns * u64::from(grid.rows)).then(|| {
        GridRegion {
            column: (index % columns) as u32,
            row: (index / columns) as u32,
            ..grid
        }
        .uv(full)
    })
}

/// Advances authored sprite-sheet flipbooks without repacking their images.
pub fn advance_sprite_animations(world: &mut World, dt: f32) {
    if !dt.is_finite() || dt <= 0.0 {
        return;
    }
    for (source, mut sprite, mut animation) in world
        .query::<(&SpriteSource, &mut Sprite, &mut SceneSpriteAnimation)>()
        .iter_mut(world)
    {
        let Some(full) = animation.image_uv.filter(|_| animation.playing) else {
            continue;
        };
        if !animation.elapsed.is_finite() || animation.elapsed < 0.0 {
            animation.elapsed = 0.0;
        }
        let duration = animation.data.frames as f32 / animation.data.fps;
        if !duration.is_finite() || duration <= 0.0 {
            continue;
        }
        animation.elapsed = if animation.data.looping {
            (animation.elapsed + dt.rem_euclid(duration)).rem_euclid(duration)
        } else {
            (animation.elapsed + dt)
                .min((animation.data.frames.saturating_sub(1)) as f32 / animation.data.fps)
        };
        if let Some(uv) = animation_uv(full, source, &animation) {
            sprite.uv = uv;
        }
    }
}

/// Time until the next visible flipbook frame, or `None` when all are idle.
pub fn next_sprite_animation_frame(world: &World) -> Option<Duration> {
    world
        .iter_entities()
        .filter(|entity| {
            entity.get::<Sprite>().is_some()
                && entity.get::<engine_ecs::components::Disabled>().is_none()
        })
        .filter_map(|entity| entity.get::<SceneSpriteAnimation>())
        .filter_map(|animation| {
            let data = animation.data;
            if data.frames <= 1
                || !animation.playing
                || data.frames > 4096
                || !data.fps.is_finite()
                || !(0.1..=120.0).contains(&data.fps)
                || !animation.elapsed.is_finite()
                || animation.elapsed < 0.0
                || animation.image_uv.is_none()
            {
                return None;
            }
            let frame = (animation.elapsed * data.fps).floor();
            if !data.looping && frame >= (data.frames - 1) as f32 {
                return None;
            }
            let delay = ((frame + 1.0) / data.fps - animation.elapsed).max(0.001);
            Some(Duration::from_secs_f32(delay))
        })
        .min()
}

/// CPU image and UV mapping ready for one GPU upload.
pub struct PackedSpriteAtlas {
    /// Atlas width in pixels.
    pub width: u32,
    /// Atlas height in pixels.
    pub height: u32,
    /// Tightly packed RGBA8 pixels, top row first.
    pub rgba: Vec<u8>,
    /// Source asset ID to its atlas UV rectangle.
    pub layout: AtlasLayout,
}

/// Builds a deterministic vertical atlas from imported scene textures.
/// A 64 MiB cap keeps a malformed or oversized project from allocating an
/// unbounded staging image during Play or editor preview.
pub fn pack_sprite_textures(
    textures: &[(&str, &ImportedTexture)],
    max_dimension: u32,
) -> Result<Option<PackedSpriteAtlas>, SceneError> {
    if textures.is_empty() {
        return Ok(None);
    }
    let mut ordered = textures.to_vec();
    ordered.sort_by_key(|(id, _)| *id);
    ordered.dedup_by_key(|(id, _)| *id);

    let width = ordered
        .iter()
        .map(|(_, image)| image.width)
        .max()
        .ok_or_else(|| SceneError::Resolve("sprite atlas has no images".into()))?;
    let height = ordered.iter().try_fold(0u32, |sum, (_, image)| {
        sum.checked_add(image.height)
            .ok_or_else(|| SceneError::Resolve("sprite atlas height overflows".into()))
    })?;
    let byte_len = usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| SceneError::Resolve("sprite atlas size overflows".into()))?;
    if width == 0
        || height == 0
        || width > max_dimension
        || height > max_dimension
        || byte_len > 64 * 1024 * 1024
    {
        return Err(SceneError::Resolve(
            "sprite atlas exceeds the supported texture size".into(),
        ));
    }

    let mut rgba = vec![0; byte_len];
    let mut layout =
        AtlasLayout::new(width, height).map_err(|error| SceneError::Resolve(error.to_string()))?;
    let mut top = 0u32;
    for (id, image) in ordered {
        let source = image
            .mip_levels
            .first()
            .ok_or_else(|| SceneError::Resolve(format!("sprite image {id} has no pixels")))?;
        let row_bytes = usize::try_from(image.width)
            .ok()
            .and_then(|width| width.checked_mul(4))
            .ok_or_else(|| SceneError::Resolve(format!("sprite image {id} is too large")))?;
        let expected = row_bytes
            .checked_mul(image.height as usize)
            .ok_or_else(|| SceneError::Resolve(format!("sprite image {id} is too large")))?;
        if image.width == 0 || image.height == 0 || source.len() != expected {
            return Err(SceneError::Resolve(format!(
                "sprite image {id} has invalid pixels"
            )));
        }
        for row in 0..image.height as usize {
            let source_start = row * row_bytes;
            let target_start = ((top as usize + row) * width as usize) * 4;
            rgba[target_start..target_start + row_bytes]
                .copy_from_slice(&source[source_start..source_start + row_bytes]);
        }
        layout
            .add_region(
                id,
                PixelRect {
                    x: 0,
                    y: top,
                    width: image.width,
                    height: image.height,
                },
            )
            .map_err(|error| SceneError::Resolve(error.to_string()))?;
        top += image.height;
    }
    Ok(Some(PackedSpriteAtlas {
        width,
        height,
        rgba,
        layout,
    }))
}

/// Assigns each resolved sprite its image's atlas rectangle. A sprite
/// whose image is unavailable becomes pending again, retaining edited
/// properties in its source component for a later import.
pub fn apply_packed_sprite_uvs(world: &mut World, layout: Option<&AtlasLayout>) -> usize {
    let mut updated = 0;
    let mut missing = Vec::new();
    for (entity, source, mut sprite, animation) in world
        .query::<(
            engine_ecs::prelude::Entity,
            &SpriteSource,
            &mut Sprite,
            Option<&mut SceneSpriteAnimation>,
        )>()
        .iter_mut(world)
    {
        let full = layout.and_then(|layout| layout.uv_rect(&source.atlas));
        let uv = if let Some(mut animation) = animation {
            animation.image_uv = full;
            full.and_then(|full| animation_uv(full, source, &animation))
        } else {
            full.and_then(|full| region_uv(full, &source.region))
        };
        if let Some(uv) = uv {
            if sprite.uv != uv {
                sprite.uv = uv;
            }
            updated += 1;
        } else {
            missing.push((entity, *sprite));
        }
    }
    for (entity, sprite) in missing {
        if let Some(mut source) = world.get_mut::<SpriteSource>(entity) {
            source.size = sprite.size.to_array();
            source.color = sprite.color;
            source.z_order = sprite.z_order;
        }
        world.entity_mut(entity).remove::<Sprite>();
    }
    updated
}

/// Loads distinct sprite images from a loose project or export bundle,
/// packs them, and remaps live sprites before their next render.
pub fn pack_world_sprite_atlas(
    world: &mut World,
    library: &AssetLibrary,
    max_dimension: u32,
) -> Result<Option<PackedSpriteAtlas>, SceneError> {
    let ids: BTreeSet<String> = world
        .query::<(&SpriteSource, &Sprite)>()
        .iter(world)
        .map(|(source, _)| source.atlas.clone())
        .collect();
    let mut images = Vec::new();
    for id in ids {
        let Some(bytes) = library.read(&id)? else {
            tracing::warn!(asset_id = %id, "sprite image is missing");
            continue;
        };
        let image = import_texture_bytes(&bytes)
            .map_err(|error| SceneError::Resolve(format!("sprite image {id}: {error}")))?;
        images.push((id, image));
    }
    let references: Vec<_> = images
        .iter()
        .map(|(id, image)| (id.as_str(), image))
        .collect();
    let packed = pack_sprite_textures(&references, max_dimension)?;
    apply_packed_sprite_uvs(world, packed.as_ref().map(|atlas| &atlas.layout));
    Ok(packed)
}

#[cfg(test)]
mod tests {
    use engine_asset::ImportedTexture;
    use engine_ecs::components::{Sprite, SpriteSource};
    use engine_ecs::prelude::World;
    use engine_renderer::UvRect;

    use super::{
        advance_sprite_animations, apply_packed_sprite_uvs, next_sprite_animation_frame,
        pack_sprite_textures, pack_world_sprite_atlas,
    };

    #[test]
    fn animated_sprite_advances_and_loops_within_packed_sheet() {
        let image = ImportedTexture {
            width: 4,
            height: 1,
            mip_levels: vec![vec![255; 16]],
        };
        let packed = pack_sprite_textures(&[("sheet", &image)], 4)
            .unwrap()
            .unwrap();
        let mut world = World::new();
        let entity = world
            .spawn((
                SpriteSource {
                    atlas: "sheet".into(),
                    region: "grid:4,1:0,0".into(),
                    size: [1.0; 2],
                    color: [1.0; 4],
                    z_order: 0.0,
                },
                Sprite::new(
                    glam::Vec2::ONE,
                    UvRect {
                        min: [0.0; 2],
                        max: [1.0; 2],
                    },
                ),
                crate::SceneSpriteAnimation::new(crate::SpriteAnimationData {
                    frames: 4,
                    fps: 2.0,
                    looping: true,
                }),
            ))
            .id();
        apply_packed_sprite_uvs(&mut world, Some(&packed.layout));
        assert_eq!(
            next_sprite_animation_frame(&world),
            Some(std::time::Duration::from_millis(500))
        );
        assert_eq!(world.get::<Sprite>(entity).unwrap().uv.min[0], 0.0);
        assert_eq!(world.get::<Sprite>(entity).unwrap().uv.max[0], 0.25);
        advance_sprite_animations(&mut world, 0.5);
        assert_eq!(world.get::<Sprite>(entity).unwrap().uv.min[0], 0.25);
        advance_sprite_animations(&mut world, 1.5);
        assert_eq!(world.get::<Sprite>(entity).unwrap().uv.min[0], 0.0);
        assert_eq!(
            world.get::<SpriteSource>(entity).unwrap().region,
            "grid:4,1:0,0"
        );
        world
            .get_mut::<crate::SceneSpriteAnimation>(entity)
            .unwrap()
            .data
            .looping = false;
        advance_sprite_animations(&mut world, 10.0);
        assert_eq!(world.get::<Sprite>(entity).unwrap().uv.min[0], 0.75);
        assert_eq!(next_sprite_animation_frame(&world), None);
        {
            let mut animation = world
                .get_mut::<crate::SceneSpriteAnimation>(entity)
                .unwrap();
            animation.data.looping = true;
            animation.elapsed = f32::NAN;
        }
        assert_eq!(next_sprite_animation_frame(&world), None);
        advance_sprite_animations(&mut world, 0.5);
        assert_eq!(world.get::<Sprite>(entity).unwrap().uv.min[0], 0.25);
    }

    fn pixel(rgba: [u8; 4]) -> ImportedTexture {
        ImportedTexture {
            width: 1,
            height: 1,
            mip_levels: vec![rgba.to_vec()],
        }
    }

    #[test]
    fn packs_distinct_sprite_images_with_stable_uvs() {
        let red = pixel([255, 0, 0, 255]);
        let green = pixel([0, 255, 0, 255]);
        let packed = pack_sprite_textures(&[("b", &green), ("a", &red)], 4)
            .expect("valid images")
            .expect("nonempty atlas");

        assert_eq!((packed.width, packed.height), (1, 2));
        assert_eq!(packed.rgba, [255, 0, 0, 255, 0, 255, 0, 255]);
        assert_eq!(packed.layout.uv_rect("a").unwrap().min, [0.0, 0.0]);
        assert_eq!(packed.layout.uv_rect("a").unwrap().max, [1.0, 0.5]);
        assert_eq!(packed.layout.uv_rect("b").unwrap().min, [0.0, 0.5]);
        assert_eq!(packed.layout.uv_rect("b").unwrap().max, [1.0, 1.0]);
    }

    #[test]
    fn rejects_atlas_taller_than_gpu_limit() {
        let red = pixel([255, 0, 0, 255]);
        assert!(pack_sprite_textures(&[("a", &red), ("b", &red)], 1).is_err());
    }

    #[test]
    fn remaps_live_sprites_to_their_image_in_packed_atlas() {
        let red = pixel([255, 0, 0, 255]);
        let green = pixel([0, 255, 0, 255]);
        let packed = pack_sprite_textures(&[("a", &red), ("b", &green)], 4)
            .unwrap()
            .unwrap();
        let mut world = World::new();
        let entity = world
            .spawn((
                SpriteSource {
                    atlas: "b".into(),
                    ..Default::default()
                },
                Sprite::new(
                    glam::Vec2::ONE,
                    UvRect {
                        min: [0.0; 2],
                        max: [1.0; 2],
                    },
                ),
            ))
            .id();

        assert_eq!(apply_packed_sprite_uvs(&mut world, Some(&packed.layout)), 1);
        assert_eq!(world.get::<Sprite>(entity).unwrap().uv.min, [0.0, 0.5]);
        assert_eq!(world.get::<Sprite>(entity).unwrap().uv.max, [1.0, 1.0]);
    }

    #[test]
    fn grid_region_samples_only_its_cell_after_atlas_packing() {
        let image = ImportedTexture {
            width: 2,
            height: 2,
            mip_levels: vec![vec![255; 16]],
        };
        let packed = pack_sprite_textures(&[("a", &image), ("b", &image)], 4)
            .unwrap()
            .unwrap();
        let mut world = World::new();
        let entity = world
            .spawn((
                SpriteSource {
                    atlas: "b".into(),
                    region: "grid:2,2:1,0".into(),
                    ..Default::default()
                },
                Sprite::new(
                    glam::Vec2::ONE,
                    UvRect {
                        min: [0.0; 2],
                        max: [1.0; 2],
                    },
                ),
            ))
            .id();

        apply_packed_sprite_uvs(&mut world, Some(&packed.layout));
        let uv = world.get::<Sprite>(entity).unwrap().uv;
        assert_eq!(uv.min, [0.5, 0.5]);
        assert_eq!(uv.max, [1.0, 0.75]);
    }

    #[test]
    fn missing_image_hides_sprite_without_losing_authored_properties() {
        let red = pixel([255, 0, 0, 255]);
        let packed = pack_sprite_textures(&[("available", &red)], 4)
            .unwrap()
            .unwrap();
        let mut world = World::new();
        let entity = world
            .spawn((
                SpriteSource {
                    atlas: "missing".into(),
                    ..Default::default()
                },
                Sprite {
                    size: glam::Vec2::new(2.0, 3.0),
                    uv: UvRect {
                        min: [0.0; 2],
                        max: [1.0; 2],
                    },
                    color: [0.25, 0.5, 0.75, 1.0],
                    z_order: 5.0,
                },
            ))
            .id();

        apply_packed_sprite_uvs(&mut world, Some(&packed.layout));
        assert!(world.get::<Sprite>(entity).is_none());
        let source = world.get::<SpriteSource>(entity).unwrap();
        assert_eq!(source.size, [2.0, 3.0]);
        assert_eq!(source.color, [0.25, 0.5, 0.75, 1.0]);
        assert_eq!(source.z_order, 5.0);
    }

    #[test]
    fn loads_sprite_image_referenced_by_live_world() {
        let root = std::env::temp_dir().join(format!("vge-sprite-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let id = "3fa85f64-5717-4562-b3fc-2c963f66afa6";
        let image = root.join("sprite.png");
        std::fs::write(
            &image,
            engine_asset::encode_png_rgba8(1, 1, &[7, 8, 9, 255]).unwrap(),
        )
        .unwrap();
        std::fs::write(
            root.join("sprite.png.meta"),
            format!("(id: \"{id}\", source_hash: 0)"),
        )
        .unwrap();
        let library = crate::AssetLibrary::index(&root).unwrap();
        let mut world = World::new();
        world.spawn((
            SpriteSource {
                atlas: id.into(),
                ..Default::default()
            },
            Sprite::new(
                glam::Vec2::ONE,
                UvRect {
                    min: [0.0; 2],
                    max: [1.0; 2],
                },
            ),
        ));

        let packed = pack_world_sprite_atlas(&mut world, &library, 4)
            .unwrap()
            .unwrap();
        assert_eq!(packed.rgba, [7, 8, 9, 255]);
        assert!(packed.layout.uv_rect(id).is_some());
        std::fs::remove_dir_all(root).unwrap();
    }
}
