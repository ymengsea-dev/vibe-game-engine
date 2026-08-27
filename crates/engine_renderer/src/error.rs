//! Error types produced by `engine_renderer`.

use thiserror::Error;

/// Errors that can occur while initializing or driving the GPU renderer.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum RendererError {
    /// No GPU adapter matching our requirements was found.
    #[error("no suitable GPU adapter found: {0}")]
    NoSuitableAdapter(String),

    /// Failed to create the wgpu surface for the window.
    #[error("failed to create GPU surface: {0}")]
    SurfaceCreation(String),

    /// Failed to request a logical device from the adapter.
    #[error("failed to request GPU device: {0}")]
    DeviceRequest(String),

    /// The surface reported no supported presentation formats for the
    /// chosen adapter (surface/adapter are incompatible).
    #[error("surface has no supported formats for this adapter")]
    NoSurfaceFormat,

    /// Requested an initial or resized surface size of zero in a
    /// dimension, which wgpu does not allow.
    #[error("surface size must be nonzero (got {width}x{height})")]
    ZeroSize {
        /// Requested width.
        width: u32,
        /// Requested height.
        height: u32,
    },

    /// [`crate::GpuContext::create_mesh`] was called with no vertices or
    /// no indices.
    #[error("mesh must have at least one vertex and one index")]
    EmptyMesh,

    /// [`crate::decode_rgba8`] (and [`crate::GpuContext::create_texture_from_bytes`])
    /// failed to decode image bytes — malformed or unsupported format.
    #[error("failed to decode image: {0}")]
    ImageDecode(String),

    /// [`crate::AtlasLayout::new`] was given a zero width or height.
    #[error("atlas size must be nonzero (got {width}x{height})")]
    InvalidAtlasSize {
        /// Requested atlas width, in pixels.
        width: u32,
        /// Requested atlas height, in pixels.
        height: u32,
    },

    /// [`crate::AtlasLayout::add_region`] was given a pixel rect that
    /// doesn't fit within the atlas's bounds, or has zero width/height.
    #[error(
        "atlas region {name:?} rect ({x},{y} {width}x{height}) doesn't fit atlas ({atlas_width}x{atlas_height})"
    )]
    AtlasRegionOutOfBounds {
        /// The region's name.
        name: String,
        /// Rect left edge, in pixels.
        x: u32,
        /// Rect top edge, in pixels.
        y: u32,
        /// Rect width, in pixels.
        width: u32,
        /// Rect height, in pixels.
        height: u32,
        /// The atlas's actual width, in pixels.
        atlas_width: u32,
        /// The atlas's actual height, in pixels.
        atlas_height: u32,
    },

    /// [`crate::AtlasLayout::add_grid`] was given zero columns/rows, or a
    /// grid that doesn't evenly divide the atlas's dimensions.
    #[error("grid {columns}x{rows} doesn't evenly divide atlas {atlas_width}x{atlas_height}")]
    InvalidAtlasGrid {
        /// Requested column count.
        columns: u32,
        /// Requested row count.
        rows: u32,
        /// The atlas's actual width, in pixels.
        atlas_width: u32,
        /// The atlas's actual height, in pixels.
        atlas_height: u32,
    },

    /// [`crate::RenderGraph::execute`] was given passes whose declared
    /// reads/writes form a cycle — nothing runs in that case.
    #[error("render graph has a cycle involving pass {pass:?}")]
    RenderGraphCycle {
        /// One pass involved in the cycle.
        pass: String,
    },
}
