//! Dependency-free performance budgets for representative editor workloads.

use std::time::{Duration, Instant};

/// Maximum allowed CPU time for one bounded terrain brush operation.
pub const TERRAIN_BRUSH_BUDGET: Duration = Duration::from_millis(25);
/// Maximum allowed CPU time for one dependency scan.
pub const DEPENDENCY_SCAN_BUDGET: Duration = Duration::from_millis(10);

/// Measures `operation` once and reports whether it stayed within `budget`.
pub fn within_budget(budget: Duration, operation: impl FnOnce()) -> (Duration, bool) {
    let start = Instant::now();
    operation();
    let elapsed = start.elapsed();
    (elapsed, elapsed <= budget)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terrain_tools::TerrainToolState;
    use engine_renderer::Heightmap;
    use glam::Vec2;

    #[test]
    fn terrain_brush_stays_within_the_editor_budget() {
        let mut map = Heightmap::new(128, 64.0).unwrap();
        let state = TerrainToolState::default();
        let (_, within) = within_budget(TERRAIN_BRUSH_BUDGET, || {
            state.apply_brush(&mut map, Vec2::ZERO);
        });
        assert!(within, "terrain brush exceeded {:?}", TERRAIN_BRUSH_BUDGET);
    }

    #[test]
    fn budgets_are_positive_and_explicit() {
        assert!(TERRAIN_BRUSH_BUDGET > Duration::ZERO);
        assert!(DEPENDENCY_SCAN_BUDGET > Duration::ZERO);
    }
}
