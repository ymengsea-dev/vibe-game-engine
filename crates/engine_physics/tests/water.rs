//! Water public API regression tests.
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "tests must fail immediately when their validated fixture is broken"
)]

use engine_physics::{BuoyancySettings, WaterSurface, WaterWave, buoyancy_force};
use glam::{Vec2, Vec3};

fn calm_surface() -> WaterSurface {
    WaterSurface::new(
        Vec3::ZERO,
        Vec2::splat(4.0),
        2.0,
        WaterWave {
            direction: Vec2::X,
            amplitude: 0.0,
            length: 4.0,
            speed: 0.0,
        },
        Vec3::new(0.5, 0.0, 0.0),
        1_000.0,
        0.5,
    )
    .unwrap_or_else(|error| panic!("test surface is valid: {error}"))
}

#[test]
fn rejects_invalid_surface_settings() {
    let wave = WaterWave {
        direction: Vec2::X,
        amplitude: 0.2,
        length: 4.0,
        speed: 1.0,
    };

    assert!(
        WaterSurface::new(
            Vec3::new(f32::NAN, 0.0, 0.0),
            Vec2::ONE,
            0.0,
            wave,
            Vec3::ZERO,
            1_000.0,
            0.0,
        )
        .is_err()
    );
    assert!(
        WaterSurface::new(
            Vec3::ZERO,
            Vec2::new(0.0, 1.0),
            0.0,
            wave,
            Vec3::ZERO,
            1_000.0,
            0.0,
        )
        .is_err()
    );
    assert!(WaterSurface::new(Vec3::ZERO, Vec2::ONE, 0.0, wave, Vec3::ZERO, 0.0, 0.0).is_err());
    assert!(
        WaterSurface::new(
            Vec3::ZERO,
            Vec2::ONE,
            0.0,
            WaterWave {
                length: f32::INFINITY,
                ..wave
            },
            Vec3::ZERO,
            1_000.0,
            0.0,
        )
        .is_err()
    );
}

#[test]
fn calm_defaults_are_valid_and_sampleable() {
    assert!(WaterSurface::default().sample(Vec3::ZERO, 0.0).is_some());
    assert!(BuoyancySettings::default().volume > 0.0);
}

#[test]
fn samples_edges_deterministically_with_a_unit_normal() {
    let surface = calm_surface();
    let edge = Vec3::new(4.0, 1.5, -4.0);
    let first = surface.sample(edge, 3.0).expect("edge belongs to surface");
    let second = surface.sample(edge, 3.0).expect("edge belongs to surface");

    assert_eq!(first, second);
    assert!((first.normal.length() - 1.0).abs() < 1e-5);
    assert_eq!(first.current, Vec3::new(0.5, 0.0, 0.0));
    assert_eq!(first.depth, 0.5);
    assert!(surface.sample(Vec3::new(4.01, 0.0, 0.0), 3.0).is_none());
}

#[test]
fn buoyancy_lifts_submerged_volume_and_leaves_dry_volume_unforced() {
    let sample = calm_surface()
        .sample(Vec3::new(0.0, 0.0, 0.0), 0.0)
        .expect("point is inside surface");
    let settings = BuoyancySettings {
        volume: 1.0,
        density: 1_000.0,
        center_offset: Vec3::ZERO,
        angular_damping: 0.0,
    };
    let gravity = Vec3::new(0.0, -9.81, 0.0);

    assert_eq!(
        buoyancy_force(sample, settings, gravity),
        Vec3::new(0.0, 9_810.0, 0.0)
    );

    let dry = calm_surface()
        .sample(Vec3::new(0.0, 2.0, 0.0), 0.0)
        .expect("point is inside surface");
    assert_eq!(buoyancy_force(dry, settings, gravity), Vec3::ZERO);
}
