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
    use engine_renderer::{TerrainSurface, TerrainTile};
    use glam::Vec2;

    #[test]
    fn terrain_brush_stays_within_the_editor_budget() {
        // One dab on a new-size tile plus rebuilding every chunk it
        // touched: the work done per pointer sample while sculpting.
        let mut surface = TerrainSurface::new(129, 64.0).unwrap();
        surface
            .insert_tile((0, 0), TerrainTile::flat(129, 64.0).unwrap())
            .unwrap();
        let state = TerrainToolState::default();
        let (_, within) = within_budget(TERRAIN_BRUSH_BUDGET, || {
            if let Some(rect) = state.apply_brush(&mut surface, Vec2::ZERO) {
                for key in engine_renderer::dirty_chunks(&surface, rect) {
                    let skirt = engine_renderer::skirt_depth(&surface, key);
                    for stride in engine_renderer::chunk_strides(129, key) {
                        std::hint::black_box(engine_renderer::chunk_mesh(
                            &surface, key, stride, skirt,
                        ));
                    }
                }
            }
        });
        assert!(within, "terrain brush exceeded {:?}", TERRAIN_BRUSH_BUDGET);
    }

    #[test]
    fn budgets_are_positive_and_explicit() {
        assert!(TERRAIN_BRUSH_BUDGET > Duration::ZERO);
        assert!(DEPENDENCY_SCAN_BUDGET > Duration::ZERO);
    }
}
