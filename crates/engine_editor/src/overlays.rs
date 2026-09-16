//! Scene-view debug overlays: colliders, lights, cameras, and the
//! ground grid, drawn as debug lines.
//!
//! ## Why this lives in the editor
//!
//! Overlays are an authoring tool. Everything here is in
//! `engine_editor`, which a runtime build never links (NFR-004), so a
//! shipped game cannot draw them by accident — there is no flag to get
//! wrong.
//!
//! ## Where the data comes from
//!
//! The editor holds no physics world, so colliders are read from the
//! [`SceneCollider`] carrier the scene format spawns, not from rapier.
//! Cameras come from the [`Camera`] component, and lights from the
//! viewport's own [`LightSet`] — the editor's lighting is a viewport
//! property, not a set of entities, so the overlay follows the data
//! rather than inventing entities to hang it on.
//!
//! Everything appends into one [`DebugDraw`], which is one vertex buffer
//! and one draw call however many overlays are on.

use engine_ecs::prelude::{Camera, Entity, GlobalTransform, Transform, World};
use engine_renderer::{DebugDraw, LightSet};
use engine_scene::{ColliderShape, SceneAudioEmitter, SceneCollider, SceneNavGrid};
use glam::{Mat4, Vec3};
use serde::{Deserialize, Serialize};

/// Colour of collider wireframes: rapier-green, the convention every
/// physics debug view uses.
const COLLIDER_COLOR: [f32; 4] = [0.35, 0.9, 0.45, 1.0];
/// Colour of camera frusta.
const CAMERA_COLOR: [f32; 4] = [0.45, 0.7, 1.0, 1.0];
/// Colour of light markers.
const LIGHT_COLOR: [f32; 4] = [1.0, 0.85, 0.35, 1.0];
/// Colour of the ground grid — dim, so it reads as a floor rather than
/// as geometry.
const GRID_COLOR: [f32; 4] = [0.35, 0.35, 0.4, 1.0];
/// Colour of audio emitters.
const AUDIO_COLOR: [f32; 4] = [0.8, 0.45, 0.95, 1.0];
/// Colour of walkable nav-grid cells.
const NAV_WALKABLE_COLOR: [f32; 4] = [0.3, 0.6, 0.5, 0.8];
/// Colour of blocked nav-grid cells — the thing you are looking for when
/// you turn this overlay on.
const NAV_BLOCKED_COLOR: [f32; 4] = [0.95, 0.35, 0.3, 1.0];
/// Height above the grid's plane that nav cells are drawn at, so they do
/// not z-fight the ground.
const NAV_LIFT: f32 = 0.02;

/// Size of a point-light marker, in world units.
const LIGHT_MARKER_SIZE: f32 = 0.35;
/// Length of the directional-light arrow.
const SUN_ARROW_LENGTH: f32 = 4.0;
/// How many cells the ground grid draws per side, and how big each is.
const GRID_CELLS: u32 = 40;
/// Size of one ground-grid cell, in world units.
const GRID_CELL_SIZE: f32 = 1.0;

/// Which Scene-view overlays are on. One flag per overlay, so each
/// toggles independently; persisted per project in the editor session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverlayToggles {
    /// Collider shapes declared by the scene.
    #[serde(default)]
    pub colliders: bool,
    /// Directional-light headings and point-light positions.
    #[serde(default)]
    pub lights: bool,
    /// Camera view volumes.
    #[serde(default)]
    pub cameras: bool,
    /// A ground grid through the world origin.
    #[serde(default)]
    pub grid: bool,
    /// Audio emitters: a marker per emitter and a ring at its radius.
    #[serde(default)]
    pub audio: bool,
    /// The scene's nav grid, walkable cells outlined and blocked cells
    /// crossed out.
    #[serde(default)]
    pub nav: bool,
}

impl OverlayToggles {
    /// Whether any overlay is on. All off draws nothing at all — no
    /// vertices, and the caller skips the draw call.
    pub fn any(self) -> bool {
        self.colliders || self.lights || self.cameras || self.grid || self.audio || self.nav
    }
}

/// Draws every enabled overlay into `draw`.
///
/// Appends; the caller clears `draw` once per frame and may add its own
/// geometry (the gizmo, placeholder boxes) to the same buffer.
pub fn build(world: &mut World, lights: &LightSet, toggles: OverlayToggles, draw: &mut DebugDraw) {
    if !toggles.any() {
        return;
    }
    if toggles.grid {
        draw.grid(Vec3::ZERO, GRID_CELLS, GRID_CELL_SIZE, GRID_COLOR);
    }
    if toggles.colliders {
        draw_colliders(world, draw);
    }
    if toggles.cameras {
        draw_cameras(world, draw);
    }
    if toggles.lights {
        draw_lights(lights, draw);
    }
    if toggles.audio {
        draw_audio_emitters(world, draw);
    }
    if toggles.nav {
        draw_nav_grid(world, draw);
    }
}

/// A marker per audio emitter, with a ring at the distance it carries.
///
/// The radius is the point of the overlay: a sound's reach is invisible
/// otherwise, and placing emitters by ear means running the game.
fn draw_audio_emitters(world: &mut World, draw: &mut DebugDraw) {
    let emitters: Vec<(Entity, f32)> = world
        .query::<(Entity, &SceneAudioEmitter)>()
        .iter(world)
        .map(|(entity, emitter)| (entity, emitter.0.radius))
        .collect();

    for (entity, radius) in emitters {
        let position = world_matrix(world, entity).transform_point3(Vec3::ZERO);
        draw.cross(position, LIGHT_MARKER_SIZE, AUDIO_COLOR);
        if radius > 0.0 {
            draw.wire_sphere(position, radius, AUDIO_COLOR);
        }
    }
}

/// The scene's nav grid, lying in its own plane.
///
/// Walkable cells are drawn as an outline and blocked ones crossed out,
/// rather than every cell as a box: a 64x64 grid is 4096 cells, and the
/// overlay has to stay readable as well as cheap.
fn draw_nav_grid(world: &mut World, draw: &mut DebugDraw) {
    let Some(grid) = world
        .get_resource::<SceneNavGrid>()
        .map(|res| res.0.clone())
    else {
        return;
    };
    if !grid.is_well_formed() {
        return;
    }

    let cell = grid.cell_size;
    let origin = Vec3::new(grid.origin[0], NAV_LIFT, grid.origin[1]);
    // Outline first — one line list for the whole grid, not per cell.
    for x in 0..=grid.width {
        let offset = x as f32 * cell;
        draw.line(
            origin + Vec3::new(offset, 0.0, 0.0),
            origin + Vec3::new(offset, 0.0, grid.height as f32 * cell),
            NAV_WALKABLE_COLOR,
        );
    }
    for z in 0..=grid.height {
        let offset = z as f32 * cell;
        draw.line(
            origin + Vec3::new(0.0, 0.0, offset),
            origin + Vec3::new(grid.width as f32 * cell, 0.0, offset),
            NAV_WALKABLE_COLOR,
        );
    }

    for (index, blocked) in grid.blocked.iter().enumerate() {
        if !blocked {
            continue;
        }
        let x = (index % grid.width as usize) as f32 * cell;
        let z = (index / grid.width as usize) as f32 * cell;
        let min = origin + Vec3::new(x, 0.0, z);
        let max = min + Vec3::new(cell, 0.0, cell);
        draw.line(min, max, NAV_BLOCKED_COLOR);
        draw.line(
            min + Vec3::new(cell, 0.0, 0.0),
            min + Vec3::new(0.0, 0.0, cell),
            NAV_BLOCKED_COLOR,
        );
    }
}

/// An entity's world matrix, preferring its computed [`GlobalTransform`]
/// and falling back to its local [`Transform`] — a scene the editor has
/// just loaded may not have propagated yet.
fn world_matrix(world: &World, entity: Entity) -> Mat4 {
    if let Some(global) = world.get::<GlobalTransform>(entity) {
        return global.0.to_matrix();
    }
    world
        .get::<Transform>(entity)
        .map_or(Mat4::IDENTITY, |local| local.0.to_matrix())
}

/// One wireframe per collider the scene declared.
fn draw_colliders(world: &mut World, draw: &mut DebugDraw) {
    let carriers: Vec<(Entity, ColliderShape)> = world
        .query::<(Entity, &SceneCollider)>()
        .iter(world)
        .map(|(entity, collider)| (entity, collider.0.shape.clone()))
        .collect();

    for (entity, shape) in carriers {
        let matrix = world_matrix(world, entity);
        let centre = matrix.transform_point3(Vec3::ZERO);
        match shape {
            ColliderShape::Ball { radius } => draw.wire_sphere(centre, radius, COLLIDER_COLOR),
            ColliderShape::Cuboid { half_extents } => {
                draw.wire_box_transformed(matrix, Vec3::from_array(half_extents), COLLIDER_COLOR)
            }
            ColliderShape::Cylinder {
                half_height,
                radius,
            } => draw.wire_cylinder(centre, half_height, radius, COLLIDER_COLOR),
            ColliderShape::Capsule {
                half_height,
                radius,
            } => draw.wire_capsule(centre, half_height, radius, COLLIDER_COLOR),
            // A triangle-mesh collider is the render mesh: drawing its
            // triangles as lines would bury the scene in wireframe, so
            // it gets a marker saying "there is a mesh collider here".
            ColliderShape::TriMesh { .. } => {
                draw.cross(centre, LIGHT_MARKER_SIZE, COLLIDER_COLOR);
            }
        }
    }
}

/// A view volume per camera entity.
fn draw_cameras(world: &mut World, draw: &mut DebugDraw) {
    let cameras: Vec<(Entity, engine_renderer::Camera)> = world
        .query::<(Entity, &Camera)>()
        .iter(world)
        .map(|(entity, camera)| (entity, camera.0))
        .collect();

    for (entity, mut camera) in cameras {
        // The entity's transform is where the camera is; a camera
        // component carries its own eye/target, so only move it if the
        // entity has been placed somewhere else.
        let matrix = world_matrix(world, entity);
        let placed = matrix.transform_point3(Vec3::ZERO);
        if placed != Vec3::ZERO {
            let offset = placed - camera.eye;
            camera.eye = placed;
            camera.target += offset;
        }
        draw.frustum(camera.view_projection_matrix(), CAMERA_COLOR);
    }
}

/// A heading arrow per directional light, a marker per point light.
fn draw_lights(lights: &LightSet, draw: &mut DebugDraw) {
    for light in &lights.directional {
        // A sun has a direction but no position: the arrow is drawn at
        // the origin so it reads as "this way", not "from here".
        draw.arrow(Vec3::ZERO, light.direction, SUN_ARROW_LENGTH, LIGHT_COLOR);
    }
    for light in &lights.point {
        let position = light.position;
        draw.cross(position, LIGHT_MARKER_SIZE, LIGHT_COLOR);
        // A ring at the light's range, so its reach is visible rather
        // than guessed.
        if light.range > 0.0 {
            draw.wire_sphere(position, light.range, LIGHT_COLOR);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_ecs::prelude::Name;
    use engine_renderer::{DirectionalLight, PointLight};
    use engine_scene::{BodyKind, ColliderData};

    fn world_with_a_box_collider(count: usize) -> World {
        let mut world = World::new();
        for index in 0..count {
            world.spawn((
                Name::new(format!("Body {index}")),
                Transform::from(engine_utils::Transform::from_translation(Vec3::new(
                    index as f32,
                    0.0,
                    0.0,
                ))),
                SceneCollider(ColliderData {
                    shape: ColliderShape::Cuboid {
                        half_extents: [1.0, 1.0, 1.0],
                    },
                    body: BodyKind::Fixed,
                }),
            ));
        }
        world
    }

    fn lit() -> LightSet {
        LightSet {
            directional: vec![DirectionalLight {
                direction: Vec3::NEG_Y,
                color: Vec3::ONE,
                intensity: 1.0,
            }],
            point: vec![PointLight {
                position: Vec3::new(0.0, 2.0, 0.0),
                color: Vec3::ONE,
                intensity: 1.0,
                range: 5.0,
            }],
            ..LightSet::default()
        }
    }

    #[test]
    fn disabled_overlay_emits_nothing() {
        let mut world = world_with_a_box_collider(4);
        let mut draw = DebugDraw::new();
        build(&mut world, &lit(), OverlayToggles::default(), &mut draw);
        assert!(
            draw.is_empty(),
            "every overlay off must cost no vertices, so the draw call is skipped"
        );
        assert!(!OverlayToggles::default().any());
    }

    #[test]
    fn each_overlay_toggles_independently() {
        let mut world = world_with_a_box_collider(2);
        world.spawn((
            Name::new("Camera"),
            Transform::default(),
            Camera::from(engine_renderer::Camera::new(
                Vec3::new(0.0, 2.0, 5.0),
                Vec3::ZERO,
                16.0 / 9.0,
            )),
        ));
        let lights = lit();

        let mut count = |toggles: OverlayToggles| {
            let mut draw = DebugDraw::new();
            build(&mut world, &lights, toggles, &mut draw);
            draw.len()
        };

        let colliders = count(OverlayToggles {
            colliders: true,
            ..Default::default()
        });
        let cameras = count(OverlayToggles {
            cameras: true,
            ..Default::default()
        });
        let lights_only = count(OverlayToggles {
            lights: true,
            ..Default::default()
        });
        let grid = count(OverlayToggles {
            grid: true,
            ..Default::default()
        });

        for (name, drawn) in [
            ("colliders", colliders),
            ("cameras", cameras),
            ("lights", lights_only),
            ("grid", grid),
        ] {
            assert!(drawn > 0, "{name} drew nothing when switched on");
        }

        // Two colliders, two boxes: turning one overlay on adds only its
        // own geometry.
        assert_eq!(colliders, 2 * 24);
        assert_eq!(cameras, 24, "one frustum is one box's worth of edges");

        // And everything together is exactly the sum — no overlay
        // suppresses or duplicates another.
        let all = count(OverlayToggles {
            colliders: true,
            cameras: true,
            lights: true,
            grid: true,
            ..Default::default()
        });
        assert_eq!(all, colliders + cameras + lights_only + grid);
    }

    #[test]
    fn audio_and_nav_overlays_draw_what_the_scene_authored() {
        use engine_scene::{AssetRef, AudioEmitterData, NavGridData};

        let mut world = World::new();
        world.spawn((
            Name::new("Waterfall"),
            Transform::from(engine_utils::Transform::from_translation(Vec3::new(
                3.0, 0.0, 0.0,
            ))),
            SceneAudioEmitter(AudioEmitterData {
                sound: AssetRef { id: "a".to_owned() },
                autoplay: true,
                looping: true,
                gain: 1.0,
                radius: 4.0,
            }),
        ));
        let mut grid = NavGridData::new(3, 3, 1.0, [0.0, 0.0]);
        grid.blocked[4] = true;
        world.insert_resource(SceneNavGrid(grid));

        let audio = {
            let mut draw = DebugDraw::new();
            build(
                &mut world,
                &LightSet::default(),
                OverlayToggles {
                    audio: true,
                    ..Default::default()
                },
                &mut draw,
            );
            draw.len()
        };
        assert!(
            audio > 0,
            "an emitter with a radius draws a marker and a ring"
        );

        let mut draw = DebugDraw::new();
        build(
            &mut world,
            &LightSet::default(),
            OverlayToggles {
                nav: true,
                ..Default::default()
            },
            &mut draw,
        );
        // 4 lines each way for a 3x3 grid, plus a cross on the one
        // blocked cell: outlines are drawn per row and column, not per
        // cell, so the overlay stays cheap on a big grid.
        assert_eq!(draw.len(), (4 + 4) * 2 + 2 * 2);
    }

    #[test]
    fn a_malformed_nav_grid_draws_nothing_rather_than_reading_past_its_cells() {
        use engine_scene::NavGridData;

        let mut world = World::new();
        let mut grid = NavGridData::new(4, 4, 1.0, [0.0, 0.0]);
        grid.blocked.truncate(3); // as a hand-edited scene file could be
        world.insert_resource(SceneNavGrid(grid));

        let mut draw = DebugDraw::new();
        build(
            &mut world,
            &LightSet::default(),
            OverlayToggles {
                nav: true,
                ..Default::default()
            },
            &mut draw,
        );
        assert!(draw.is_empty());
    }

    #[test]
    fn a_thousand_colliders_stay_in_one_buffer() {
        let mut world = world_with_a_box_collider(1000);
        let mut draw = DebugDraw::new();
        build(
            &mut world,
            &LightSet::default(),
            OverlayToggles {
                colliders: true,
                ..Default::default()
            },
            &mut draw,
        );
        // One vertex list, so one draw call — 1000 boxes, 24 vertices
        // each, and nothing per-shape beyond that.
        assert_eq!(draw.len(), 1000 * 24);
        assert_eq!(draw.vertices().len(), draw.len());
    }

    #[test]
    fn a_collider_is_drawn_where_its_entity_is() {
        let mut world = world_with_a_box_collider(1);
        let mut draw = DebugDraw::new();
        build(
            &mut world,
            &LightSet::default(),
            OverlayToggles {
                colliders: true,
                ..Default::default()
            },
            &mut draw,
        );
        // The single entity sits at the origin with half-extents of 1,
        // so every vertex is a corner of that box.
        for vertex in draw.vertices() {
            assert!(vertex.position[0].abs() <= 1.0 + f32::EPSILON);
            assert!(vertex.position[1].abs() <= 1.0 + f32::EPSILON);
        }
    }
}
