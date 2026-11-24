//! Splits a [`TerrainSurface`] into chunks with level-of-detail meshes.
//!
//! Each tile is cut into chunks of [`CHUNK_CELLS`] cells per side (the last
//! row or column of chunks is smaller when the tile's cell count doesn't
//! divide evenly). Each chunk has up to four detail tiers that keep every
//! 1st, 2nd, 4th or 8th node; a tier is offered only when its step divides
//! the chunk's width and height.
//!
//! Neighbouring chunks may draw different tiers, so their edges don't line
//! up exactly. Every tier carries a skirt: a strip hanging down from each
//! edge, deep enough to cover the largest gap two tiers can leave there.
//! Skirts need no knowledge of what the neighbour draws.
//!
//! Vertices are in terrain-local space with the tile offset built in, so
//! one model transform draws the whole terrain. UVs run 0 to 1 across each
//! tile, which is how the terrain shader finds the tile's paint texels.
//!
//! Pure CPU math, no `wgpu`.

use std::collections::BTreeSet;

use crate::lod::select_lod_tier;
use crate::mesh::Vertex;
use crate::terrain_surface::{DirtyRect, TerrainSurface, TileCoord};

/// Cells per chunk side.
pub const CHUNK_CELLS: u32 = 32;
/// Node steps of the detail tiers, most detailed first.
pub const TIER_STRIDES: [u32; 4] = [1, 2, 4, 8];

/// One chunk: its tile and its column/row among the tile's chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChunkKey {
    /// The tile it belongs to.
    pub tile: TileCoord,
    /// Chunk column within the tile.
    pub cx: u32,
    /// Chunk row within the tile.
    pub cz: u32,
}

/// Chunks per tile side for a tile with `resolution` nodes per side.
pub fn chunks_per_side(resolution: u32) -> u32 {
    (resolution - 1).div_ceil(CHUNK_CELLS)
}

/// Local cell range `[start, end]` of chunk `index` along one axis.
fn chunk_span(resolution: u32, index: u32) -> (u32, u32) {
    let cells = resolution - 1;
    let start = (index * CHUNK_CELLS).min(cells);
    (start, ((index + 1) * CHUNK_CELLS).min(cells))
}

/// Every chunk of every tile, in order.
pub fn all_chunks(surface: &TerrainSurface) -> Vec<ChunkKey> {
    let per_side = chunks_per_side(surface.resolution());
    surface
        .tiles()
        .flat_map(|(coord, _)| {
            let coord = *coord;
            (0..per_side).flat_map(move |cz| {
                (0..per_side).map(move |cx| ChunkKey {
                    tile: coord,
                    cx,
                    cz,
                })
            })
        })
        .collect()
}

/// The steps from [`TIER_STRIDES`] this chunk can be drawn at.
pub fn chunk_strides(resolution: u32, key: ChunkKey) -> Vec<u32> {
    let (c0, c1) = chunk_span(resolution, key.cx);
    let (r0, r1) = chunk_span(resolution, key.cz);
    let (width, height) = (c1 - c0, r1 - r0);
    TIER_STRIDES
        .into_iter()
        .filter(|stride| width % stride == 0 && height % stride == 0)
        .collect()
}

/// Terrain-local edge length of a full chunk.
pub fn chunk_world_size(surface: &TerrainSurface) -> f32 {
    surface.cell_size() * CHUNK_CELLS.min(surface.resolution() - 1) as f32
}

/// Distances where a chunk switches from tier 0 to 1, 1 to 2 and 2 to 3:
/// two, four and eight chunk widths, scaled by `detail_distance`.
pub fn terrain_lod_switch_distances(chunk_world_size: f32, detail_distance: f32) -> [f32; 3] {
    let scale = if detail_distance.is_finite() && detail_distance > 0.0 {
        detail_distance
    } else {
        1.0
    };
    [2.0, 4.0, 8.0].map(|chunks| chunks * chunk_world_size * scale)
}

/// The tier a chunk with `tiers` tiers draws at `distance`, with the same
/// hysteresis as mesh level of detail.
pub fn terrain_chunk_tier(switch: &[f32; 3], tiers: usize, distance: f32, current: usize) -> usize {
    select_lod_tier(switch, tiers, distance, current)
}

/// How far below a chunk's edges its skirts hang: twice the largest
/// difference between the full-detail edge and any tier's straighter edge,
/// plus half a cell.
pub fn skirt_depth(surface: &TerrainSurface, key: ChunkKey) -> f32 {
    let resolution = surface.resolution();
    let (row_base, col_base) = surface.tile_origin(key.tile);
    let (c0, c1) = chunk_span(resolution, key.cx);
    let (r0, r1) = chunk_span(resolution, key.cz);
    let height = |row: u32, col: u32| {
        surface
            .node_height(row_base + i64::from(row), col_base + i64::from(col))
            .unwrap_or(0.0)
    };
    let mut deviation = 0.0f32;
    // Each edge as a list of node heights.
    let edges: [Vec<f32>; 4] = [
        (c0..=c1).map(|c| height(r0, c)).collect(),
        (c0..=c1).map(|c| height(r1, c)).collect(),
        (r0..=r1).map(|r| height(r, c0)).collect(),
        (r0..=r1).map(|r| height(r, c1)).collect(),
    ];
    for edge in &edges {
        let cells = edge.len() - 1;
        for stride in TIER_STRIDES {
            let stride = stride as usize;
            if stride > cells || cells % stride != 0 {
                continue;
            }
            for (index, value) in edge.iter().enumerate() {
                let start = index / stride * stride;
                let end = (start + stride).min(cells);
                let t = (index - start) as f32 / stride as f32;
                let straight = edge[start] + (edge[end] - edge[start]) * t;
                deviation = deviation.max((value - straight).abs());
            }
        }
    }
    deviation * 2.0 + surface.cell_size() * 0.5
}

/// One tier of one chunk as renderer geometry, skirts included. `stride`
/// should come from [`chunk_strides`].
pub fn chunk_mesh(
    surface: &TerrainSurface,
    key: ChunkKey,
    stride: u32,
    skirt: f32,
) -> (Vec<Vertex>, Vec<u32>) {
    let resolution = surface.resolution();
    let last = (resolution - 1) as f32;
    let (row_base, col_base) = surface.tile_origin(key.tile);
    let (c0, c1) = chunk_span(resolution, key.cx);
    let (r0, r1) = chunk_span(resolution, key.cz);
    let stride = stride.max(1);
    let cols: Vec<u32> = (c0..=c1).step_by(stride as usize).collect();
    let rows: Vec<u32> = (r0..=r1).step_by(stride as usize).collect();
    let vertex = |row: u32, col: u32, drop: f32| {
        let (gr, gc) = (row_base + i64::from(row), col_base + i64::from(col));
        let xz = surface.node_position(gr, gc);
        let height = surface.node_height(gr, gc).unwrap_or(0.0);
        Vertex {
            position: [xz.x, height - drop, xz.y],
            normal: surface.node_normal(gr, gc).to_array(),
            uv: [col as f32 / last, row as f32 / last],
        }
    };

    let width = cols.len() as u32;
    let mut vertices =
        Vec::with_capacity(rows.len() * cols.len() + 4 * cols.len().max(rows.len()) * 2);
    for &row in &rows {
        for &col in &cols {
            vertices.push(vertex(row, col, 0.0));
        }
    }
    let mut indices = Vec::new();
    for r in 0..rows.len() as u32 - 1 {
        for c in 0..width - 1 {
            let tl = r * width + c;
            let tr = tl + 1;
            let bl = tl + width;
            let br = bl + 1;
            // Same split and winding as `Heightmap::indices`.
            indices.extend_from_slice(&[tl, bl, br, tl, br, tr]);
        }
    }

    // Skirts: lowered copies of each edge, drawn from both sides so they
    // hide gaps whichever way the camera looks.
    let edges: [Vec<(u32, u32)>; 4] = [
        cols.iter().map(|&c| (r0, c)).collect(),
        cols.iter().map(|&c| (r1, c)).collect(),
        rows.iter().map(|&r| (r, c0)).collect(),
        rows.iter().map(|&r| (r, c1)).collect(),
    ];
    for edge in edges {
        let start = vertices.len() as u32;
        for &(row, col) in &edge {
            vertices.push(vertex(row, col, 0.0));
        }
        for &(row, col) in &edge {
            vertices.push(vertex(row, col, skirt));
        }
        let count = edge.len() as u32;
        for i in 0..count - 1 {
            let (a, b) = (start + i, start + i + 1);
            let (a_low, b_low) = (a + count, b + count);
            indices.extend_from_slice(&[a, a_low, b_low, a, b_low, b]);
            indices.extend_from_slice(&[a, b_low, a_low, a, b, b_low]);
        }
    }
    (vertices, indices)
}

/// Chunks whose meshes a change to `rect` (global nodes) affects. The
/// rectangle grows by one node first because a node's normal depends on
/// its neighbours.
pub fn dirty_chunks(surface: &TerrainSurface, rect: DirtyRect) -> BTreeSet<ChunkKey> {
    let rect = rect.grow(1);
    let resolution = surface.resolution();
    let per_side = chunks_per_side(resolution);
    let range = |lo: i64, hi: i64| {
        // A node on a chunk boundary belongs to the chunks on both sides.
        let cells = i64::from(CHUNK_CELLS);
        let first = if lo > 0 && lo % cells == 0 {
            lo / cells - 1
        } else {
            lo / cells
        };
        let last = (hi / cells).min(i64::from(per_side) - 1);
        (first.max(0) as u32, last.max(0) as u32)
    };
    let mut keys = BTreeSet::new();
    for coord in surface.tiles_touching(rect) {
        let Some(local) = surface.tile_local_rect(coord, rect) else {
            continue;
        };
        let (cx0, cx1) = range(local.col_min, local.col_max);
        let (cz0, cz1) = range(local.row_min, local.row_max);
        for cz in cz0..=cz1 {
            for cx in cx0..=cx1 {
                keys.insert(ChunkKey {
                    tile: coord,
                    cx,
                    cz,
                });
            }
        }
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terrain::Brush;
    use crate::terrain_surface::TerrainTile;
    use glam::{Vec2, Vec3};

    fn surface(coords: &[TileCoord], resolution: u32, size: f32) -> TerrainSurface {
        let mut surface = TerrainSurface::new(resolution, size).unwrap();
        for coord in coords {
            surface
                .insert_tile(*coord, TerrainTile::flat(resolution, size).unwrap())
                .unwrap();
        }
        surface
    }

    fn bumpy(coords: &[TileCoord], resolution: u32, size: f32) -> TerrainSurface {
        let mut s = surface(coords, resolution, size);
        let mut seed = 3u64;
        for _ in 0..30 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let x = (seed >> 40) as f32 / (1u64 << 24) as f32;
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let z = (seed >> 40) as f32 / (1u64 << 24) as f32;
            s.raise_lower(&Brush::new(
                Vec2::new(x * size * 2.0 - size * 0.5, z * size - size * 0.5),
                3.0,
                1.7,
            ));
        }
        s
    }

    #[test]
    fn tiers_have_the_expected_vertex_counts_including_skirts() {
        let s = surface(&[(0, 0)], 129, 64.0);
        let key = ChunkKey {
            tile: (0, 0),
            cx: 1,
            cz: 2,
        };
        assert_eq!(chunk_strides(129, key), vec![1, 2, 4, 8]);
        for (stride, side) in [(1u32, 33usize), (2, 17), (4, 9), (8, 5)] {
            let (vertices, indices) = chunk_mesh(&s, key, stride, 1.0);
            assert_eq!(vertices.len(), side * side + 4 * side * 2);
            let grid = (side - 1).pow(2) * 6;
            let skirts = 4 * (side - 1) * 12;
            assert_eq!(indices.len(), grid + skirts);
            assert!(indices.iter().all(|&i| (i as usize) < vertices.len()));
        }
        assert_eq!(all_chunks(&s).len(), 16);
    }

    #[test]
    fn edge_vertices_match_between_neighbouring_chunks_and_tiles() {
        let s = bumpy(&[(0, 0), (1, 0)], 65, 32.0);
        let grid = |key: ChunkKey| {
            let (vertices, _) = chunk_mesh(&s, key, 1, 0.5);
            vertices[..33 * 33].to_vec()
        };
        // Chunk (0,0) and (1,0) inside tile (0,0); then across the tile edge.
        for (left, right) in [
            (
                ChunkKey {
                    tile: (0, 0),
                    cx: 0,
                    cz: 1,
                },
                ChunkKey {
                    tile: (0, 0),
                    cx: 1,
                    cz: 1,
                },
            ),
            (
                ChunkKey {
                    tile: (0, 0),
                    cx: 1,
                    cz: 0,
                },
                ChunkKey {
                    tile: (1, 0),
                    cx: 0,
                    cz: 0,
                },
            ),
        ] {
            let (a, b) = (grid(left), grid(right));
            for row in 0..33 {
                let east = a[row * 33 + 32];
                let west = b[row * 33];
                assert_eq!(east.position, west.position, "row {row}");
                assert_eq!(east.normal, west.normal, "row {row}");
            }
        }
    }

    #[test]
    fn skirt_depth_covers_the_largest_gap_between_tiers() {
        let s = bumpy(&[(0, 0)], 65, 32.0);
        for key in all_chunks(&s) {
            let depth = skirt_depth(&s, key);
            let edge = |stride: u32| {
                let (vertices, _) = chunk_mesh(&s, key, stride, 0.0);
                let side = 32 / stride as usize + 1;
                // Top edge heights along the full-detail node spacing.
                (0..=32)
                    .map(|col| {
                        let start = col / stride as usize;
                        let end = (start + 1).min(side - 1);
                        let t = (col % stride as usize) as f32 / stride as f32;
                        let a = vertices[start].position[1];
                        let b = vertices[end].position[1];
                        a + (b - a) * t
                    })
                    .collect::<Vec<f32>>()
            };
            let tiers: Vec<Vec<f32>> = [1, 2, 4, 8].map(edge).into();
            for a in &tiers {
                for b in &tiers {
                    for (x, y) in a.iter().zip(b) {
                        assert!(
                            (x - y).abs() <= depth,
                            "{key:?}: gap {} > {depth}",
                            (x - y).abs()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_partial_chunk_offers_only_the_steps_that_fit() {
        // 9 nodes: one 8-cell chunk, every step fits.
        let small = ChunkKey {
            tile: (0, 0),
            cx: 0,
            cz: 0,
        };
        assert_eq!(chunks_per_side(9), 1);
        assert_eq!(chunk_strides(9, small), vec![1, 2, 4, 8]);
        // 51 nodes = 50 cells: one 32-cell chunk and one 18-cell chunk.
        assert_eq!(chunks_per_side(51), 2);
        assert_eq!(chunk_strides(51, ChunkKey { cx: 1, ..small }), vec![1, 2]);
        assert_eq!(
            chunk_strides(
                51,
                ChunkKey {
                    cx: 1,
                    cz: 1,
                    ..small
                }
            ),
            vec![1, 2]
        );
        let s = surface(&[(0, 0)], 51, 25.0);
        let (vertices, _) = chunk_mesh(&s, ChunkKey { cx: 1, ..small }, 2, 0.1);
        // 18 / 2 + 1 = 10 columns by 32 / 2 + 1 = 17 rows.
        assert_eq!(vertices.len(), 10 * 17 + 2 * (10 + 10) + 2 * (17 + 17));
    }

    #[test]
    fn a_brush_at_a_chunk_corner_dirties_the_four_chunks_around_it() {
        let mut s = surface(&[(0, 0)], 129, 64.0);
        // Node (32, 32) is the shared corner of chunks (0,0), (1,0), (0,1),
        // (1,1); at 0.5 m per cell it sits at (-16, -16).
        let rect = s
            .raise_lower(&Brush::new(Vec2::new(-16.0, -16.0), 0.4, 1.0))
            .unwrap();
        let keys = dirty_chunks(&s, rect);
        let expected: BTreeSet<_> = [(0, 0), (1, 0), (0, 1), (1, 1)]
            .map(|(cx, cz)| ChunkKey {
                tile: (0, 0),
                cx,
                cz,
            })
            .into();
        assert_eq!(keys, expected);
        // A brush inside one chunk away from its edges dirties only it.
        let inner = s
            .raise_lower(&Brush::new(Vec2::new(-24.0, -24.0), 1.0, 1.0))
            .unwrap();
        assert_eq!(dirty_chunks(&s, inner).len(), 1);
    }

    #[test]
    fn an_edit_on_a_tile_edge_dirties_chunks_in_both_tiles() {
        let mut s = surface(&[(0, 0), (1, 0)], 65, 32.0);
        let rect = s
            .raise_lower(&Brush::new(Vec2::new(16.0, -8.0), 0.3, 1.0))
            .unwrap();
        let tiles: BTreeSet<_> = dirty_chunks(&s, rect).into_iter().map(|k| k.tile).collect();
        assert_eq!(tiles, [(0, 0), (1, 0)].into());
    }

    #[test]
    fn chunk_tier_switching_follows_mesh_lod_hysteresis() {
        let switch = terrain_lod_switch_distances(16.0, 1.0);
        assert_eq!(switch, [32.0, 64.0, 128.0]);
        assert_eq!(terrain_chunk_tier(&switch, 4, 10.0, 0), 0);
        assert_eq!(terrain_chunk_tier(&switch, 4, 34.0, 0), 0);
        assert_eq!(terrain_chunk_tier(&switch, 4, 36.0, 0), 1);
        assert_eq!(terrain_chunk_tier(&switch, 4, 30.0, 1), 1);
        assert_eq!(terrain_chunk_tier(&switch, 4, 500.0, 0), 3);
        assert_eq!(terrain_chunk_tier(&switch, 2, 500.0, 0), 1);
        assert_eq!(terrain_lod_switch_distances(16.0, f32::NAN), switch);
        assert_eq!(terrain_lod_switch_distances(16.0, 2.0)[0], 64.0);
    }

    #[test]
    fn chunk_triangles_face_up() {
        let s = bumpy(&[(0, 0)], 33, 16.0);
        let (vertices, indices) = chunk_mesh(
            &s,
            ChunkKey {
                tile: (0, 0),
                cx: 0,
                cz: 0,
            },
            1,
            0.0,
        );
        let p = |i: u32| Vec3::from(vertices[i as usize].position);
        let grid = 32 * 32 * 6;
        for tri in indices[..grid].as_chunks::<3>().0 {
            assert!((p(tri[1]) - p(tri[0])).cross(p(tri[2]) - p(tri[0])).y > 0.0);
        }
    }
}
