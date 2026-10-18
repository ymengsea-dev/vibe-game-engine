//! Fixed-step water interaction and buoyancy.

use bevy_ecs::{
    entity::Entity,
    prelude::{Component, Resource},
    world::World,
};
use engine_physics::{BuoyancySettings, PhysicsWorld, WaterSample, buoyancy_force};
use glam::Vec3;

use crate::components::{RigidBody, Transform};

/// Opts an entity into buoyancy forces when it has a dynamic rigid body.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct BuoyantBody {
    /// Geometry-neutral displaced volume settings.
    pub settings: BuoyancySettings,
}

/// Tracks an entity that needs water entry, exit, and movement information.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct WaterInteractor {
    previous_surface: Option<Entity>,
    previous_position: Vec3,
}

impl Default for WaterInteractor {
    fn default() -> Self {
        Self {
            previous_surface: None,
            previous_position: Vec3::ZERO,
        }
    }
}

/// A game-facing water interaction.
#[allow(missing_docs, reason = "variant documentation defines field meaning")]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WaterInteractionEvent {
    /// Entity entered a surface.
    Entered {
        entity: Entity,
        surface: Entity,
        position: Vec3,
        normal: Vec3,
        immersion: f32,
    },
    /// Entity left its prior surface.
    Exited {
        entity: Entity,
        surface: Entity,
        position: Vec3,
    },
    /// Entity moved while immersed.
    Moved {
        entity: Entity,
        surface: Entity,
        position: Vec3,
        normal: Vec3,
        speed: f32,
        immersion: f32,
    },
}

/// Events emitted by [`update_water`] since the last render/gameplay drain.
#[derive(Resource, Debug, Default)]
pub struct WaterEvents(pub Vec<WaterInteractionEvent>);

/// Samples water, emits interaction events, and applies dynamic-body forces.
pub fn update_water(world: &mut World, physics: &mut PhysicsWorld, time: f32, fixed_dt: f32) {
    if !time.is_finite() || !fixed_dt.is_finite() || fixed_dt <= 0.0 {
        return;
    }
    if world.get_resource::<WaterEvents>().is_none() {
        world.insert_resource(WaterEvents::default());
    }
    let surfaces: Vec<_> = world
        .query::<(Entity, &crate::components::WaterVolume)>()
        .iter(world)
        .map(|(entity, volume)| (entity, volume.surface))
        .collect();
    let mut events = Vec::new();
    let mut query = world.query::<(
        Entity,
        &Transform,
        Option<&RigidBody>,
        Option<&BuoyantBody>,
        Option<&mut WaterInteractor>,
    )>();
    for (entity, transform, rigid_body, buoyant, interactor) in query.iter_mut(world) {
        let position = transform.0.translation;
        let found = surfaces.iter().find_map(|(surface, water)| {
            water
                .sample(position, time)
                .map(|sample| (*surface, sample))
        });
        let buoyancy_sample = buoyant.and_then(|body| {
            let sample_position = position + body.settings.center_offset;
            surfaces
                .iter()
                .find_map(|(_, water)| water.sample(sample_position, time))
        });
        if let Some(mut interactor) = interactor {
            match (interactor.previous_surface, found) {
                (None, Some((surface, sample))) => events.push(WaterInteractionEvent::Entered {
                    entity,
                    surface,
                    position,
                    normal: sample.normal,
                    immersion: immersion(sample),
                }),
                (Some(previous), None) => events.push(WaterInteractionEvent::Exited {
                    entity,
                    surface: previous,
                    position,
                }),
                (Some(previous), Some((surface, sample))) if previous == surface => {
                    let speed = position.distance(interactor.previous_position) / fixed_dt;
                    if speed > 0.01 {
                        events.push(WaterInteractionEvent::Moved {
                            entity,
                            surface,
                            position,
                            normal: sample.normal,
                            speed,
                            immersion: immersion(sample),
                        });
                    }
                }
                (Some(previous), Some((surface, sample))) => {
                    events.push(WaterInteractionEvent::Exited {
                        entity,
                        surface: previous,
                        position,
                    });
                    events.push(WaterInteractionEvent::Entered {
                        entity,
                        surface,
                        position,
                        normal: sample.normal,
                        immersion: immersion(sample),
                    });
                }
                (None, None) => {}
            }
            interactor.previous_surface = found.map(|(surface, _)| surface);
            interactor.previous_position = position;
        }
        if let (Some(rigid_body), Some(buoyant), Some(sample)) =
            (rigid_body, buoyant, buoyancy_sample)
            && let Some(body) = physics.rapier.bodies.get_mut(rigid_body.0)
            && body.is_dynamic()
        {
            let force = buoyancy_force(sample, buoyant.settings, physics.rapier.gravity);
            if force != Vec3::ZERO {
                body.add_force(force, true);
            }
            let drag = -(body.linvel() - sample.current) * sample.linear_drag * immersion(sample);
            body.add_force(drag, true);
            body.set_angular_damping(buoyant.settings.angular_damping);
        }
    }
    world.resource_mut::<WaterEvents>().0.extend(events);
}

fn immersion(sample: WaterSample) -> f32 {
    sample.depth.max(0.0).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use engine_physics::{ColliderBuilder, RigidBodyBuilder, WaterSurface, WaterWave};
    use engine_utils::Transform as MathTransform;
    use glam::{Vec2, Vec3};

    use super::*;
    use crate::components::WaterVolume;

    fn surface() -> WaterSurface {
        WaterSurface::new(
            Vec3::ZERO,
            Vec2::splat(4.0),
            1.0,
            WaterWave {
                direction: Vec2::X,
                amplitude: 0.0,
                length: 2.0,
                speed: 0.0,
            },
            Vec3::ZERO,
            1000.0,
            1.0,
        )
        .unwrap()
    }

    #[test]
    fn emits_enter_move_and_exit_once() {
        let mut world = World::new();
        let water = world.spawn(WaterVolume { surface: surface() }).id();
        let actor = world
            .spawn((
                Transform(MathTransform::from_translation(Vec3::ZERO)),
                WaterInteractor::default(),
            ))
            .id();
        let mut physics = PhysicsWorld::default();

        update_water(&mut world, &mut physics, 0.0, 1.0);
        update_water(&mut world, &mut physics, 1.0, 1.0);
        {
            let events = world.resource::<WaterEvents>();
            assert_eq!(events.0.len(), 1);
            assert!(
                matches!(events.0[0], WaterInteractionEvent::Entered { surface, .. } if surface == water)
            );
        }
        world.resource_mut::<WaterEvents>().0.clear();

        world.get_mut::<Transform>(actor).unwrap().0.translation.x = 1.0;
        update_water(&mut world, &mut physics, 2.0, 1.0);
        assert!(matches!(
            world.resource::<WaterEvents>().0.as_slice(),
            [WaterInteractionEvent::Moved { speed, immersion, .. }] if *speed > 0.0 && *immersion > 0.0
        ));
        world.resource_mut::<WaterEvents>().0.clear();

        world.get_mut::<Transform>(actor).unwrap().0.translation.x = 5.0;
        update_water(&mut world, &mut physics, 3.0, 1.0);
        assert!(matches!(
            world.resource::<WaterEvents>().0.as_slice(),
            [WaterInteractionEvent::Exited { surface, .. }] if *surface == water
        ));
    }

    #[test]
    fn kinematic_body_receives_events_without_buoyant_force() {
        let mut world = World::new();
        world.spawn(WaterVolume { surface: surface() });
        let mut physics = PhysicsWorld::default();
        let (body, _) = RigidBody::spawn(
            &mut physics,
            RigidBodyBuilder::kinematic_position_based(),
            ColliderBuilder::ball(0.5),
        );
        world.spawn((
            Transform(MathTransform::from_translation(Vec3::ZERO)),
            body,
            BuoyantBody {
                settings: BuoyancySettings {
                    volume: 1.0,
                    density: 500.0,
                    center_offset: Vec3::ZERO,
                    angular_damping: 1.0,
                },
            },
            WaterInteractor::default(),
        ));

        update_water(&mut world, &mut physics, 0.0, 1.0 / 60.0);
        assert!(matches!(
            world.resource::<WaterEvents>().0.as_slice(),
            [WaterInteractionEvent::Entered { .. }]
        ));
        assert_eq!(
            physics.rapier.bodies.get(body.0).unwrap().linvel(),
            Vec3::ZERO
        );
    }

    #[test]
    fn submerged_dynamic_body_receives_upward_force() {
        let mut world = World::new();
        world.spawn(WaterVolume { surface: surface() });
        let mut physics = PhysicsWorld::default();
        let (body, _) = RigidBody::spawn(
            &mut physics,
            RigidBodyBuilder::dynamic(),
            ColliderBuilder::ball(0.5),
        );
        world.spawn((
            Transform(MathTransform::from_translation(Vec3::ZERO)),
            body,
            BuoyantBody {
                settings: BuoyancySettings {
                    volume: 1.0,
                    density: 500.0,
                    center_offset: Vec3::ZERO,
                    angular_damping: 1.0,
                },
            },
        ));

        update_water(&mut world, &mut physics, 0.0, 1.0 / 60.0);
        physics.step(1.0 / 60.0).unwrap();
        assert!(physics.rapier.bodies.get(body.0).unwrap().linvel().y > 0.0);
    }

    #[test]
    fn buoyancy_samples_the_configured_center_offset() {
        let mut world = World::new();
        world.spawn(WaterVolume { surface: surface() });
        let mut physics = PhysicsWorld::default();
        let (body, _) = RigidBody::spawn(
            &mut physics,
            RigidBodyBuilder::dynamic(),
            ColliderBuilder::ball(0.5),
        );
        world.spawn((
            Transform(MathTransform::from_translation(Vec3::new(0.0, 2.0, 0.0))),
            body,
            BuoyantBody {
                settings: BuoyancySettings {
                    volume: 1.0,
                    density: 500.0,
                    center_offset: Vec3::new(0.0, -2.0, 0.0),
                    angular_damping: 1.0,
                },
            },
        ));

        update_water(&mut world, &mut physics, 0.0, 1.0 / 60.0);
        physics.step(1.0 / 60.0).unwrap();
        assert!(physics.rapier.bodies.get(body.0).unwrap().linvel().y > 0.0);
    }

    #[test]
    fn stale_body_handle_is_ignored() {
        let mut world = World::new();
        world.spawn(WaterVolume { surface: surface() });
        let mut physics = PhysicsWorld::default();
        let (body, _) = RigidBody::spawn(
            &mut physics,
            RigidBodyBuilder::dynamic(),
            ColliderBuilder::ball(0.5),
        );
        physics.rapier.remove_body(body.0);
        world.spawn((
            Transform(MathTransform::from_translation(Vec3::ZERO)),
            body,
            BuoyantBody {
                settings: BuoyancySettings {
                    volume: 1.0,
                    density: 500.0,
                    center_offset: Vec3::ZERO,
                    angular_damping: 1.0,
                },
            },
        ));

        update_water(&mut world, &mut physics, 0.0, 1.0 / 60.0);
    }
}
