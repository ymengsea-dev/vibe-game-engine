//! 2D collision checks: axis-aligned bounding boxes and circles.
//!
//! Deliberately separate from this crate's rapier3d-backed simulation —
//! plain `glam::Vec2` math, no rapier dependency, no rigid bodies, no
//! simulation step. A lightweight boolean "do these two shapes overlap"
//! check for 2D games, not a physics engine; `rapier2d` is the documented
//! escalation path if a validation game proves this isn't enough
//! (Engine Architecture Audit's Stage 2 rationale — see `ROADMAP.md`).
//!
//! None of these types validate at construction (same precedent as
//! [`crate::PhysicsWorld`]'s neighbors in `engine_renderer`, e.g.
//! `Camera`): they're built directly in code each frame from live
//! positions, not deserialized untrusted input. A negative
//! `half_extents`/`radius` describes a degenerate shape and produces
//! meaningless-but-not-panicking results ("garbage in, garbage out") —
//! every check here avoids `glam`'s assert-guarded `Vec2::clamp` for
//! exactly this reason (see [`aabb_vs_circle`]).

use glam::Vec2;

/// An axis-aligned bounding box in 2D, described by its center and
/// half-extents (half width/height along each axis) — moving it (e.g.
/// following an entity's position each frame) is just overwriting
/// `center`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aabb2d {
    /// Center position.
    pub center: Vec2,
    /// Half-extents along X and Y. Each should be `>= 0`; see the module
    /// docs for what a negative value does.
    pub half_extents: Vec2,
}

impl Aabb2d {
    /// An AABB centered at `center`, `size.x` wide and `size.y` tall.
    pub fn new(center: Vec2, size: Vec2) -> Self {
        Self {
            center,
            half_extents: size / 2.0,
        }
    }

    /// This AABB's minimum corner (`center - half_extents`).
    pub fn min(&self) -> Vec2 {
        self.center - self.half_extents
    }

    /// This AABB's maximum corner (`center + half_extents`).
    pub fn max(&self) -> Vec2 {
        self.center + self.half_extents
    }
}

/// A circle in 2D, described by its center and radius.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Circle2d {
    /// Center position.
    pub center: Vec2,
    /// Radius. Should be `>= 0`; see the module docs for what a negative
    /// value does.
    pub radius: f32,
}

impl Circle2d {
    /// A circle centered at `center` with the given `radius`.
    pub fn new(center: Vec2, radius: f32) -> Self {
        Self { center, radius }
    }
}

/// `true` if `a` and `b` overlap. Touching edges count as overlapping
/// (`<=`, not `<`).
pub fn aabb_vs_aabb(a: &Aabb2d, b: &Aabb2d) -> bool {
    let (a_min, a_max) = (a.min(), a.max());
    let (b_min, b_max) = (b.min(), b.max());
    a_min.x <= b_max.x && a_max.x >= b_min.x && a_min.y <= b_max.y && a_max.y >= b_min.y
}

/// `true` if `a` and `b` overlap. Touching (distance exactly equal to the
/// sum of radii) counts as overlapping.
pub fn circle_vs_circle(a: &Circle2d, b: &Circle2d) -> bool {
    let radius_sum = a.radius + b.radius;
    a.center.distance_squared(b.center) <= radius_sum * radius_sum
}

/// `true` if `aabb` and `circle` overlap, via the closest point on `aabb`
/// to `circle`'s center.
///
/// Clamps `circle.center` into `aabb`'s bounds by hand
/// (`.max(min).min(max)`) rather than calling `glam::Vec2::clamp`, whose
/// `min <= max` debug assertion would panic for a degenerate
/// (negative-half-extents) `aabb` — see the module docs.
pub fn aabb_vs_circle(aabb: &Aabb2d, circle: &Circle2d) -> bool {
    let closest = circle.center.max(aabb.min()).min(aabb.max());
    closest.distance_squared(circle.center) <= circle.radius * circle.radius
}

/// Either 2D collision shape, for storing or comparing colliders without
/// knowing which concrete shape each one is ahead of time (e.g. a
/// heterogeneous list of level obstacles).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Collider2d {
    /// An axis-aligned bounding box.
    Aabb(Aabb2d),
    /// A circle.
    Circle(Circle2d),
}

impl Collider2d {
    /// `true` if `self` and `other` overlap, dispatching to
    /// [`aabb_vs_aabb`], [`circle_vs_circle`], or [`aabb_vs_circle`] as
    /// needed.
    pub fn intersects(&self, other: &Collider2d) -> bool {
        match (self, other) {
            (Collider2d::Aabb(a), Collider2d::Aabb(b)) => aabb_vs_aabb(a, b),
            (Collider2d::Circle(a), Collider2d::Circle(b)) => circle_vs_circle(a, b),
            (Collider2d::Aabb(a), Collider2d::Circle(b)) => aabb_vs_circle(a, b),
            (Collider2d::Circle(a), Collider2d::Aabb(b)) => aabb_vs_circle(b, a),
        }
    }

    /// This collider's center, regardless of shape.
    pub fn center(&self) -> Vec2 {
        match self {
            Collider2d::Aabb(aabb) => aabb.center,
            Collider2d::Circle(circle) => circle.center,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aabb_new_computes_half_extents_from_size() {
        let aabb = Aabb2d::new(Vec2::new(1.0, 2.0), Vec2::new(4.0, 6.0));
        assert_eq!(aabb.half_extents, Vec2::new(2.0, 3.0));
        assert_eq!(aabb.min(), Vec2::new(-1.0, -1.0));
        assert_eq!(aabb.max(), Vec2::new(3.0, 5.0));
    }

    #[test]
    fn aabb_vs_aabb_overlapping_is_true() {
        let a = Aabb2d::new(Vec2::ZERO, Vec2::splat(2.0));
        let b = Aabb2d::new(Vec2::new(1.0, 0.0), Vec2::splat(2.0));
        assert!(aabb_vs_aabb(&a, &b));
    }

    #[test]
    fn aabb_vs_aabb_touching_edges_is_true() {
        // a spans x in [-1, 1], b spans x in [1, 3] — edges touch exactly.
        let a = Aabb2d::new(Vec2::ZERO, Vec2::splat(2.0));
        let b = Aabb2d::new(Vec2::new(2.0, 0.0), Vec2::splat(2.0));
        assert!(aabb_vs_aabb(&a, &b));
    }

    #[test]
    fn aabb_vs_aabb_separated_is_false() {
        let a = Aabb2d::new(Vec2::ZERO, Vec2::splat(2.0));
        let b = Aabb2d::new(Vec2::new(10.0, 0.0), Vec2::splat(2.0));
        assert!(!aabb_vs_aabb(&a, &b));
    }

    #[test]
    fn aabb_vs_aabb_nested_is_true() {
        let outer = Aabb2d::new(Vec2::ZERO, Vec2::splat(10.0));
        let inner = Aabb2d::new(Vec2::ZERO, Vec2::splat(1.0));
        assert!(aabb_vs_aabb(&outer, &inner));
    }

    #[test]
    fn aabb_vs_aabb_negative_half_extents_does_not_panic() {
        let a = Aabb2d {
            center: Vec2::ZERO,
            half_extents: Vec2::splat(-1.0),
        };
        let b = Aabb2d::new(Vec2::ZERO, Vec2::splat(2.0));
        // No assertion on the (meaningless) result — only that this
        // doesn't panic.
        let _ = aabb_vs_aabb(&a, &b);
    }

    #[test]
    fn circle_vs_circle_overlapping_is_true() {
        let a = Circle2d::new(Vec2::ZERO, 2.0);
        let b = Circle2d::new(Vec2::new(3.0, 0.0), 2.0);
        assert!(circle_vs_circle(&a, &b));
    }

    #[test]
    fn circle_vs_circle_touching_is_true() {
        let a = Circle2d::new(Vec2::ZERO, 1.0);
        let b = Circle2d::new(Vec2::new(2.0, 0.0), 1.0);
        assert!(circle_vs_circle(&a, &b));
    }

    #[test]
    fn circle_vs_circle_separated_is_false() {
        let a = Circle2d::new(Vec2::ZERO, 1.0);
        let b = Circle2d::new(Vec2::new(10.0, 0.0), 1.0);
        assert!(!circle_vs_circle(&a, &b));
    }

    #[test]
    fn circle_vs_circle_same_center_is_true() {
        let a = Circle2d::new(Vec2::new(5.0, 5.0), 1.0);
        let b = Circle2d::new(Vec2::new(5.0, 5.0), 3.0);
        assert!(circle_vs_circle(&a, &b));
    }

    #[test]
    fn circle_vs_circle_negative_radius_does_not_panic() {
        let a = Circle2d::new(Vec2::ZERO, -5.0);
        let b = Circle2d::new(Vec2::ZERO, 1.0);
        let _ = circle_vs_circle(&a, &b);
    }

    #[test]
    fn aabb_vs_circle_center_inside_box_is_true() {
        let aabb = Aabb2d::new(Vec2::ZERO, Vec2::splat(4.0));
        let circle = Circle2d::new(Vec2::new(0.5, 0.5), 0.1);
        assert!(aabb_vs_circle(&aabb, &circle));
    }

    #[test]
    fn aabb_vs_circle_overlapping_edge_is_true() {
        // aabb spans x in [-1, 1]; circle centered just outside at x=1.5,
        // radius 1.0 reaches back to x=0.5.
        let aabb = Aabb2d::new(Vec2::ZERO, Vec2::splat(2.0));
        let circle = Circle2d::new(Vec2::new(1.5, 0.0), 1.0);
        assert!(aabb_vs_circle(&aabb, &circle));
    }

    #[test]
    fn aabb_vs_circle_just_past_corner_is_true() {
        // aabb's corner is at (1, 1); circle centered a hair closer than
        // its radius along the diagonal — robustly overlapping regardless
        // of `normalize()`'s floating-point rounding (unlike testing
        // exact tangency, which can round either side of the `<=`).
        let aabb = Aabb2d::new(Vec2::ZERO, Vec2::splat(2.0));
        let radius = 1.0;
        let corner = Vec2::new(1.0, 1.0);
        let center = corner + corner.normalize() * (radius - 1e-4);
        let circle = Circle2d::new(center, radius);
        assert!(aabb_vs_circle(&aabb, &circle));
    }

    #[test]
    fn aabb_vs_circle_just_beyond_corner_is_false() {
        let aabb = Aabb2d::new(Vec2::ZERO, Vec2::splat(2.0));
        let radius = 1.0;
        let corner = Vec2::new(1.0, 1.0);
        let center = corner + corner.normalize() * (radius + 1e-2);
        let circle = Circle2d::new(center, radius);
        assert!(!aabb_vs_circle(&aabb, &circle));
    }

    #[test]
    fn aabb_vs_circle_far_away_is_false() {
        let aabb = Aabb2d::new(Vec2::ZERO, Vec2::splat(2.0));
        let circle = Circle2d::new(Vec2::new(100.0, 100.0), 1.0);
        assert!(!aabb_vs_circle(&aabb, &circle));
    }

    #[test]
    fn aabb_vs_circle_negative_half_extents_does_not_panic() {
        let aabb = Aabb2d {
            center: Vec2::ZERO,
            half_extents: Vec2::splat(-1.0),
        };
        let circle = Circle2d::new(Vec2::ZERO, 1.0);
        let _ = aabb_vs_circle(&aabb, &circle);
    }

    #[test]
    fn collider2d_intersects_aabb_vs_aabb() {
        let a = Collider2d::Aabb(Aabb2d::new(Vec2::ZERO, Vec2::splat(2.0)));
        let b = Collider2d::Aabb(Aabb2d::new(Vec2::new(1.0, 0.0), Vec2::splat(2.0)));
        assert!(a.intersects(&b));
    }

    #[test]
    fn collider2d_intersects_circle_vs_circle() {
        let a = Collider2d::Circle(Circle2d::new(Vec2::ZERO, 2.0));
        let b = Collider2d::Circle(Circle2d::new(Vec2::new(1.0, 0.0), 2.0));
        assert!(a.intersects(&b));
    }

    #[test]
    fn collider2d_intersects_aabb_vs_circle_both_orders() {
        let aabb = Collider2d::Aabb(Aabb2d::new(Vec2::ZERO, Vec2::splat(4.0)));
        let circle = Collider2d::Circle(Circle2d::new(Vec2::new(0.5, 0.5), 0.1));
        assert!(aabb.intersects(&circle));
        assert!(circle.intersects(&aabb));
    }

    #[test]
    fn collider2d_intersects_false_when_far_apart() {
        let a = Collider2d::Aabb(Aabb2d::new(Vec2::ZERO, Vec2::splat(1.0)));
        let b = Collider2d::Circle(Circle2d::new(Vec2::new(100.0, 100.0), 1.0));
        assert!(!a.intersects(&b));
    }

    #[test]
    fn collider2d_center_matches_shape() {
        let aabb = Collider2d::Aabb(Aabb2d::new(Vec2::new(1.0, 2.0), Vec2::splat(2.0)));
        let circle = Collider2d::Circle(Circle2d::new(Vec2::new(3.0, 4.0), 1.0));
        assert_eq!(aabb.center(), Vec2::new(1.0, 2.0));
        assert_eq!(circle.center(), Vec2::new(3.0, 4.0));
    }
}
