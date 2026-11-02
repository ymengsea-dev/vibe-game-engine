//! Scene View icons for objects that have a place in the world but no
//! visible shape: cameras, point lights and positional sound emitters.
//!
//! Icons are painted over the viewport image at a constant size (like the
//! gizmo), so they stay readable at any distance, and they are clickable to
//! select their object. Collecting and picking are pure and unit-tested; the
//! shell supplies the camera matrix and paints with egui.

use engine_ecs::components::{Disabled, Lock};
use engine_ecs::prelude::{Camera, Entity, World};
use engine_scene::{SceneAudioEmitter, ScenePointLight};
use glam::{Mat4, Vec2, Vec3};

use crate::gizmo::project_to_screen;
use crate::hierarchy::world_transform;

/// Radius of an icon's round badge, in pixels; also its click target.
pub const ICON_RADIUS_PX: f32 = 13.0;

/// What an icon depicts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconKind {
    /// A scene camera.
    Camera,
    /// A point light.
    Light,
    /// A positional sound emitter.
    Sound,
}

/// One icon to draw: its object, kind and world position.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SceneIcon {
    /// The object the icon selects.
    pub entity: Entity,
    /// The symbol drawn.
    pub kind: IconKind,
    /// World-space position of the object.
    pub position: Vec3,
}

/// Every enabled, unlocked camera, point light and sound emitter, one icon each
/// (a camera wins over a light, a light over a sound, if one object is several).
pub fn collect(world: &mut World) -> Vec<SceneIcon> {
    let mut query = world.query::<(
        Entity,
        Option<&Camera>,
        Option<&ScenePointLight>,
        Option<&SceneAudioEmitter>,
        Option<&Disabled>,
        Option<&Lock>,
    )>();
    let tagged: Vec<(Entity, IconKind)> = query
        .iter(world)
        .filter(|(.., disabled, locked)| disabled.is_none() && locked.is_none())
        .filter_map(|(entity, camera, light, sound, ..)| {
            let kind = if camera.is_some() {
                IconKind::Camera
            } else if light.is_some() {
                IconKind::Light
            } else if sound.is_some() {
                IconKind::Sound
            } else {
                return None;
            };
            Some((entity, kind))
        })
        .collect();
    tagged
        .into_iter()
        .map(|(entity, kind)| SceneIcon {
            entity,
            kind,
            position: world_transform(world, entity).translation,
        })
        .collect()
}

/// Icons projected to viewport-local pixels; icons behind the camera are dropped.
pub fn project(icons: &[SceneIcon], view_proj: Mat4, screen: Vec2) -> Vec<(SceneIcon, Vec2)> {
    icons
        .iter()
        .filter_map(|icon| Some((*icon, project_to_screen(view_proj, icon.position, screen)?)))
        .collect()
}

/// The icon under `pointer`, preferring the closest when several overlap.
pub fn pick(projected: &[(SceneIcon, Vec2)], pointer: Vec2) -> Option<Entity> {
    projected
        .iter()
        .map(|(icon, at)| (icon.entity, at.distance(pointer)))
        .filter(|(_, distance)| *distance <= ICON_RADIUS_PX)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(entity, _)| entity)
}

/// Paints one icon badge centred at `at`.
pub fn paint(painter: &egui::Painter, kind: IconKind, at: egui::Pos2, selected: bool) {
    let palette = crate::theme::NIGHT;
    let ink = palette.text;
    painter.circle_filled(at, ICON_RADIUS_PX, palette.shell.gamma_multiply(0.85));
    let ring = if selected {
        egui::Stroke::new(2.0, palette.highlight)
    } else {
        egui::Stroke::new(1.0, egui::Color32::from_white_alpha(70))
    };
    painter.circle_stroke(at, ICON_RADIUS_PX, ring);
    let stroke = egui::Stroke::new(1.6, ink);
    match kind {
        IconKind::Camera => {
            let body =
                egui::Rect::from_center_size(at + egui::vec2(-2.5, 0.0), egui::vec2(11.0, 8.0));
            painter.rect_filled(body, 1.5, ink);
            painter.add(egui::Shape::convex_polygon(
                vec![
                    at + egui::vec2(3.0, 0.0),
                    at + egui::vec2(8.0, -4.0),
                    at + egui::vec2(8.0, 4.0),
                ],
                ink,
                egui::Stroke::NONE,
            ));
        }
        IconKind::Light => {
            painter.circle_filled(at, 3.5, palette.highlight);
            for step in 0..8 {
                let angle = step as f32 * std::f32::consts::FRAC_PI_4;
                let direction = egui::vec2(angle.cos(), angle.sin());
                painter.line_segment([at + direction * 5.5, at + direction * 8.5], stroke);
            }
        }
        IconKind::Sound => {
            painter.add(egui::Shape::convex_polygon(
                vec![
                    at + egui::vec2(-7.0, -2.5),
                    at + egui::vec2(-4.5, -2.5),
                    at + egui::vec2(-0.5, -6.0),
                    at + egui::vec2(-0.5, 6.0),
                    at + egui::vec2(-4.5, 2.5),
                    at + egui::vec2(-7.0, 2.5),
                ],
                ink,
                egui::Stroke::NONE,
            ));
            for radius in [4.0, 7.0] {
                let points: Vec<_> = (-3..=3)
                    .map(|step| {
                        let angle = step as f32 * 0.22;
                        at + egui::vec2(1.0 + radius * angle.cos(), radius * angle.sin())
                    })
                    .collect();
                painter.add(egui::Shape::line(points, stroke));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_ecs::components::{GlobalTransform, Transform};
    use engine_utils::Transform as MathTransform;

    fn at(x: f32) -> (Transform, GlobalTransform) {
        let transform = MathTransform::from_translation(Vec3::new(x, 0.0, 0.0));
        (Transform::from(transform), GlobalTransform::from(transform))
    }

    #[test]
    fn only_cameras_lights_and_sounds_get_icons() {
        let mut world = World::new();
        let light = world
            .spawn((
                at(1.0),
                ScenePointLight(engine_scene::PointLightData::default()),
            ))
            .id();
        let sound = world
            .spawn((
                at(2.0),
                SceneAudioEmitter(engine_scene::AudioEmitterData::new(
                    engine_scene::AssetRef {
                        id: uuid::Uuid::new_v4().to_string(),
                    },
                )),
            ))
            .id();
        world.spawn(at(3.0)); // An empty object: no icon.
        world.spawn((
            at(4.0),
            ScenePointLight(engine_scene::PointLightData::default()),
            Disabled,
        ));
        world.spawn((
            at(5.0),
            ScenePointLight(engine_scene::PointLightData::default()),
            Lock,
        ));

        let mut icons = collect(&mut world);
        icons.sort_by(|a, b| a.position.x.total_cmp(&b.position.x));
        assert_eq!(
            icons
                .iter()
                .map(|icon| (icon.entity, icon.kind))
                .collect::<Vec<_>>(),
            vec![(light, IconKind::Light), (sound, IconKind::Sound)]
        );
        assert_eq!(icons[0].position, Vec3::X);
    }

    #[test]
    fn clicking_selects_the_nearest_icon_within_its_badge() {
        let mut world = World::new();
        let a = world.spawn_empty().id();
        let b = world.spawn_empty().id();
        let icon = |entity| SceneIcon {
            entity,
            kind: IconKind::Light,
            position: Vec3::ZERO,
        };
        let projected = vec![
            (icon(a), Vec2::new(100.0, 100.0)),
            (icon(b), Vec2::new(110.0, 100.0)),
        ];
        assert_eq!(pick(&projected, Vec2::new(108.0, 101.0)), Some(b));
        assert_eq!(pick(&projected, Vec2::new(98.0, 100.0)), Some(a));
        assert_eq!(pick(&projected, Vec2::new(200.0, 200.0)), None);
    }

    #[test]
    fn icons_behind_the_camera_are_not_drawn() {
        let mut world = World::new();
        let entity = world.spawn_empty().id();
        let view_proj = engine_renderer::Camera::new(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, 1.0)
            .view_projection_matrix();
        let icon = |z| SceneIcon {
            entity,
            kind: IconKind::Camera,
            position: Vec3::new(0.0, 0.0, z),
        };
        let projected = project(&[icon(0.0), icon(10.0)], view_proj, Vec2::splat(200.0));
        assert_eq!(projected.len(), 1);
        assert!(
            projected[0].1.distance(Vec2::splat(100.0)) < 1e-3,
            "origin is centred"
        );
    }
}
