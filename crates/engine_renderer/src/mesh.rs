//! Vertex format, GPU mesh buffers, and built-in reference geometry
//! ([`cube`] for 3D, [`quad`] for 2D sprites).

use glam::Vec3;

use crate::bounds::Aabb;
use crate::error::RendererError;
use crate::gpu::GpuContext;

/// A single mesh vertex: position, normal, and texture coordinate.
///
/// This is the one vertex layout every mesh in the engine uses for now
/// (no per-material vertex formats yet). `#[repr(C)]` +
/// [`bytemuck::Pod`]/[`bytemuck::Zeroable`] make it safely castable to
/// bytes for GPU upload, the same pattern as [`crate::CameraUniform`].
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    /// Object-space position.
    pub position: [f32; 3],
    /// Object-space normal (unit length).
    pub normal: [f32; 3],
    /// Texture coordinate (`[u, v]`, `[0,0]` top-left by convention).
    pub uv: [f32; 2],
}

impl Vertex {
    const ATTRIBUTES: [wgpu::VertexAttribute; 3] =
        wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2];

    /// This vertex format's wgpu buffer layout, for use when building a
    /// render pipeline's vertex state.
    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: size_of::<Vertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBUTES,
        }
    }
}

/// A GPU-resident mesh: a vertex buffer, an index buffer, how many
/// vertices/indices it holds, and the object-space bounds of its geometry.
pub struct Mesh {
    /// Packed [`Vertex`] data.
    pub vertex_buffer: wgpu::Buffer,
    /// `u32` triangle-list indices into `vertex_buffer`.
    pub index_buffer: wgpu::Buffer,
    /// Number of vertices in `vertex_buffer`. Fixed for the mesh's
    /// lifetime — [`GpuContext::write_mesh_vertices`] rewrites the buffer
    /// contents but can't resize it.
    pub vertex_count: u32,
    /// Number of indices to draw (`index_buffer` holds exactly this many
    /// `u32`s).
    pub index_count: u32,
    /// The tightest [`Aabb`] around this mesh's vertex positions, in
    /// object space. Computed at upload from the same `vertices` slice the
    /// buffers are built from (and recomputed by
    /// [`GpuContext::write_mesh_vertices`]); transformed by an entity's
    /// model matrix each frame ([`Aabb::transformed`]) to get its
    /// world-space extent for frustum culling.
    pub local_bounds: Aabb,
}

/// The tightest [`Aabb`] around `vertices`' positions, or a zero-size box
/// at the origin if `vertices` is empty (callers reject empty meshes
/// before this).
fn bounds_of(vertices: &[Vertex]) -> Aabb {
    Aabb::from_points(vertices.iter().map(|v| Vec3::from(v.position))).unwrap_or(Aabb {
        min: Vec3::ZERO,
        max: Vec3::ZERO,
    })
}

impl GpuContext {
    /// Uploads `vertices`/`indices` as a new GPU-resident [`Mesh`] whose
    /// vertex buffer is immutable after creation.
    ///
    /// # Errors
    ///
    /// Returns [`RendererError::EmptyMesh`] if either slice is empty —
    /// wgpu allows zero-sized buffers, but an empty mesh is never useful
    /// and is far more likely a caller bug than an intentional draw of
    /// nothing.
    pub fn create_mesh(
        &self,
        label: &str,
        vertices: &[Vertex],
        indices: &[u32],
    ) -> Result<Mesh, RendererError> {
        self.create_mesh_impl(label, vertices, indices, wgpu::BufferUsages::VERTEX)
    }

    /// Like [`GpuContext::create_mesh`], but the vertex buffer also gets
    /// `COPY_DST` so its contents can be rewritten in place later with
    /// [`GpuContext::write_mesh_vertices`] — for meshes that change every
    /// few frames without changing vertex count, e.g. a sculpted
    /// [`crate::Heightmap`]'s terrain mesh.
    ///
    /// # Errors
    ///
    /// Returns [`RendererError::EmptyMesh`] if either slice is empty.
    pub fn create_mesh_dynamic(
        &self,
        label: &str,
        vertices: &[Vertex],
        indices: &[u32],
    ) -> Result<Mesh, RendererError> {
        self.create_mesh_impl(
            label,
            vertices,
            indices,
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        )
    }

    fn create_mesh_impl(
        &self,
        label: &str,
        vertices: &[Vertex],
        indices: &[u32],
        vertex_usage: wgpu::BufferUsages,
    ) -> Result<Mesh, RendererError> {
        use wgpu::util::DeviceExt;

        if vertices.is_empty() || indices.is_empty() {
            return Err(RendererError::EmptyMesh);
        }

        let local_bounds = bounds_of(vertices);

        let vertex_buffer = self
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(&format!("{label} vertices")),
                contents: bytemuck::cast_slice(vertices),
                usage: vertex_usage,
            });
        let index_buffer = self.create_index_buffer(&format!("{label} indices"), indices);

        Ok(Mesh {
            vertex_buffer,
            index_buffer,
            vertex_count: vertices.len() as u32,
            index_count: indices.len() as u32,
            local_bounds,
        })
    }

    /// Rewrites `mesh`'s vertex buffer in place with `vertices` and
    /// recomputes its [`Mesh::local_bounds`]. `mesh` must have been created
    /// with [`GpuContext::create_mesh_dynamic`].
    ///
    /// The index buffer is untouched — this is for meshes whose topology
    /// is fixed and only vertex positions/normals move (terrain sculpting).
    ///
    /// # Errors
    ///
    /// - [`RendererError::EmptyMesh`] if `vertices` is empty.
    /// - [`RendererError::MeshVertexCountMismatch`] if `vertices.len()`
    ///   differs from `mesh.vertex_count` — an in-place write can't resize
    ///   the buffer.
    pub fn write_mesh_vertices(
        &self,
        mesh: &mut Mesh,
        vertices: &[Vertex],
    ) -> Result<(), RendererError> {
        if vertices.is_empty() {
            return Err(RendererError::EmptyMesh);
        }
        if vertices.len() as u32 != mesh.vertex_count {
            return Err(RendererError::MeshVertexCountMismatch {
                expected: mesh.vertex_count,
                got: vertices.len() as u32,
            });
        }

        self.queue()
            .write_buffer(&mesh.vertex_buffer, 0, bytemuck::cast_slice(vertices));
        mesh.local_bounds = bounds_of(vertices);
        Ok(())
    }
}

/// Vertex/index data for a unit cube centered on the origin (extents
/// `[-0.5, 0.5]` on each axis), with per-face normals and `[0,1]`-range
/// UVs tiled once per face.
///
/// Reference geometry for exercising the mesh/render pipeline before a
/// real asset pipeline exists (Milestone 5).
pub fn cube() -> (Vec<Vertex>, Vec<u32>) {
    // Four corner positions plus a shared flat normal, per face.
    type Face = ([f32; 3], [f32; 3], [f32; 3], [f32; 3], [f32; 3]);

    // 6 faces * 4 vertices (not shared across faces, so each face keeps
    // its own flat normal and full [0,1] UV range).
    const FACES: [Face; 6] = [
        // +X
        (
            [0.5, -0.5, -0.5],
            [0.5, -0.5, 0.5],
            [0.5, 0.5, 0.5],
            [0.5, 0.5, -0.5],
            [1.0, 0.0, 0.0],
        ),
        // -X
        (
            [-0.5, -0.5, 0.5],
            [-0.5, -0.5, -0.5],
            [-0.5, 0.5, -0.5],
            [-0.5, 0.5, 0.5],
            [-1.0, 0.0, 0.0],
        ),
        // +Y
        (
            [-0.5, 0.5, -0.5],
            [0.5, 0.5, -0.5],
            [0.5, 0.5, 0.5],
            [-0.5, 0.5, 0.5],
            [0.0, 1.0, 0.0],
        ),
        // -Y
        (
            [-0.5, -0.5, 0.5],
            [0.5, -0.5, 0.5],
            [0.5, -0.5, -0.5],
            [-0.5, -0.5, -0.5],
            [0.0, -1.0, 0.0],
        ),
        // +Z
        (
            [-0.5, -0.5, 0.5],
            [0.5, -0.5, 0.5],
            [0.5, 0.5, 0.5],
            [-0.5, 0.5, 0.5],
            [0.0, 0.0, 1.0],
        ),
        // -Z
        (
            [0.5, -0.5, -0.5],
            [-0.5, -0.5, -0.5],
            [-0.5, 0.5, -0.5],
            [0.5, 0.5, -0.5],
            [0.0, 0.0, -1.0],
        ),
    ];
    const FACE_UVS: [[f32; 2]; 4] = [[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]];

    let mut vertices = Vec::with_capacity(24);
    let mut indices = Vec::with_capacity(36);

    for (a, b, c, d, normal) in FACES {
        let base = vertices.len() as u32;
        for (position, uv) in [a, b, c, d].into_iter().zip(FACE_UVS) {
            vertices.push(Vertex {
                position,
                normal,
                uv,
            });
        }
        // Two CCW triangles per face (front-facing when viewed from
        // outside the cube, matching wgpu's default front-face winding).
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    (vertices, indices)
}

/// Vertex/index data for a unit quad centered on the origin in the XY
/// plane (extents `[-0.5, 0.5]` on X and Y, `z = 0`), standard `[0,1]` UV,
/// CCW winding (front-facing under wgpu's default).
///
/// Reference geometry for [`crate::SpritePipeline`] — one of these is
/// uploaded once (via [`GpuContext::create_mesh`]) and stamped out per
/// instance by [`crate::SpriteInstance`]'s position/size/rotation; the
/// per-vertex `normal` here is unused (sprites are unlit) but reusing
/// [`Vertex`] means reusing [`GpuContext::create_mesh`] and its
/// [`crate::RendererError::EmptyMesh`] validation rather than a parallel
/// 2D-only vertex type and mesh constructor.
pub fn quad() -> (Vec<Vertex>, Vec<u32>) {
    let normal = [0.0, 0.0, 1.0];
    let vertices = vec![
        Vertex {
            position: [-0.5, -0.5, 0.0],
            normal,
            uv: [0.0, 1.0],
        },
        Vertex {
            position: [0.5, -0.5, 0.0],
            normal,
            uv: [1.0, 1.0],
        },
        Vertex {
            position: [0.5, 0.5, 0.0],
            normal,
            uv: [1.0, 0.0],
        },
        Vertex {
            position: [-0.5, 0.5, 0.0],
            normal,
            uv: [0.0, 0.0],
        },
    ];
    let indices = vec![0, 1, 2, 0, 2, 3];
    (vertices, indices)
}

impl GpuContext {
    fn create_index_buffer(&self, label: &str, indices: &[u32]) -> wgpu::Buffer {
        use wgpu::util::DeviceExt;
        self.device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: bytemuck::cast_slice(indices),
                usage: wgpu::BufferUsages::INDEX,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vertex_layout_stride_matches_struct_size() {
        assert_eq!(
            Vertex::layout().array_stride,
            size_of::<Vertex>() as wgpu::BufferAddress
        );
    }

    #[test]
    fn vertex_layout_has_three_attributes() {
        assert_eq!(Vertex::layout().attributes.len(), 3);
    }

    #[test]
    fn cube_has_24_vertices_and_36_indices() {
        let (vertices, indices) = cube();
        assert_eq!(vertices.len(), 24);
        assert_eq!(indices.len(), 36);
    }

    #[test]
    fn cube_indices_are_all_in_bounds() {
        let (vertices, indices) = cube();
        assert!(indices.iter().all(|&i| (i as usize) < vertices.len()));
    }

    #[test]
    fn cube_vertices_stay_within_unit_extents() {
        let (vertices, _) = cube();
        for v in vertices {
            for component in v.position {
                assert!((-0.5..=0.5).contains(&component));
            }
        }
    }

    #[test]
    fn cube_face_normals_are_unit_length() {
        let (vertices, _) = cube();
        for v in vertices {
            let len_sq: f32 = v.normal.iter().map(|c| c * c).sum();
            assert!((len_sq - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn quad_has_4_vertices_and_6_indices() {
        let (vertices, indices) = quad();
        assert_eq!(vertices.len(), 4);
        assert_eq!(indices.len(), 6);
    }

    #[test]
    fn quad_indices_are_all_in_bounds() {
        let (vertices, indices) = quad();
        assert!(indices.iter().all(|&i| (i as usize) < vertices.len()));
    }

    #[test]
    fn quad_vertices_stay_within_unit_extents_and_z_zero() {
        let (vertices, _) = quad();
        for v in vertices {
            assert!((-0.5..=0.5).contains(&v.position[0]));
            assert!((-0.5..=0.5).contains(&v.position[1]));
            assert_eq!(v.position[2], 0.0);
        }
    }

    #[test]
    fn quad_uvs_cover_the_full_0_1_range() {
        let (vertices, _) = quad();
        let us: Vec<f32> = vertices.iter().map(|v| v.uv[0]).collect();
        let vs: Vec<f32> = vertices.iter().map(|v| v.uv[1]).collect();
        assert!(us.contains(&0.0) && us.contains(&1.0));
        assert!(vs.contains(&0.0) && vs.contains(&1.0));
    }

    // `create_mesh` needs a live GPU device, but the `local_bounds` it
    // stores is exactly `super::bounds_of` over the vertex positions —
    // exercise that computation directly on the built-in geometry.

    #[test]
    fn cube_local_bounds_are_the_unit_extents() {
        let (vertices, _) = cube();
        let bounds = bounds_of(&vertices);
        assert_eq!(bounds.min, Vec3::splat(-0.5));
        assert_eq!(bounds.max, Vec3::splat(0.5));
    }

    #[test]
    fn quad_local_bounds_are_flat_on_z() {
        let (vertices, _) = quad();
        let bounds = bounds_of(&vertices);
        assert_eq!(bounds.min, Vec3::new(-0.5, -0.5, 0.0));
        assert_eq!(bounds.max, Vec3::new(0.5, 0.5, 0.0));
    }
}
