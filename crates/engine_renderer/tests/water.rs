//! Water renderer public API regression tests.

use engine_renderer::{MAX_WATER_IMPULSES, WaterImpulse, WaterImpulseQueue, WaterUniform};
use glam::{Vec2, Vec3};

#[test]
fn uniform_normalizes_direction_and_queue_drops_oldest_impulse() {
    let uniform = WaterUniform::new(
        Vec3::new(1.0, 2.0, 3.0),
        Vec2::new(4.0, 5.0),
        Vec2::new(3.0, 4.0),
        0.25,
        8.0,
        1.5,
    );
    assert_eq!(uniform.base_height(), 2.0);
    assert_eq!(uniform.extent(), Vec2::new(4.0, 5.0));
    assert!((uniform.wave_direction().length() - 1.0).abs() < 1e-5);

    let mut impulses = WaterImpulseQueue::new(7);
    for age in 0..=MAX_WATER_IMPULSES {
        impulses.push(WaterImpulse::new(Vec3::X, 1.0, 1.0, age as f32));
    }
    assert_eq!(impulses.len(), MAX_WATER_IMPULSES);
    assert_eq!(impulses.as_slice()[0].age, 1.0);
}

#[test]
fn impulse_queue_ages_and_removes_expired_impulses() {
    let mut impulses = WaterImpulseQueue::new(7);
    impulses.push(WaterImpulse::new(Vec3::ZERO, 1.0, 1.0, 0.0));
    impulses.advance(0.5, 1.0);
    assert_eq!(impulses.as_slice()[0].age, 0.5);

    impulses.advance(0.5, 1.0);
    assert!(impulses.is_empty());
}

#[test]
fn gpu_uniform_has_the_shader_required_alignment() {
    assert_eq!(std::mem::size_of::<engine_renderer::WaterGpuUniform>(), 80);
}

#[test]
fn gpu_uniform_never_contains_non_finite_values() {
    let uniform = engine_renderer::WaterGpuUniform::new(
        Vec3::splat(f32::NAN),
        Vec2::splat(f32::INFINITY),
        Vec2::splat(f32::NAN),
        f32::NAN,
        f32::INFINITY,
        f32::NAN,
        f32::INFINITY,
    );
    assert!(uniform.center_height.iter().all(|value| value.is_finite()));
    assert!(uniform.extent_wave.iter().all(|value| value.is_finite()));
    assert!(uniform.direction_time.iter().all(|value| value.is_finite()));
}
