//! GPU vertex skinning: a [`SkinnedVertex`] format carrying per-vertex
//! joint indices/weights, a [`SkinnedMesh`] holding that geometry, a
//! [`JointMatricesUniform`] of per-joint skinning matrices, and a
//! [`SkinnedPipeline`] whose vertex shader blends those matrices by weight
//! to deform the mesh before the usual camera/model transform.
//!
//! The skinning *matrices* themselves come from the CPU —
//! `engine_animation::compute_skinning_matrices` turns a sampled pose plus
//! a skeleton into `Vec<Mat4>`, which [`JointMatricesUniform::from_matrices`]
//! packs for upload. This module is only the GPU-resident half.
//!
//! Reuses [`Pipeline`]'s camera (`@group(0)`), material (`@group(1)`), and
//! lights (`@group(3)`) bind group layouts unchanged; the joint matrices
//! sit at `@group(2) @binding(1)`, next to the per-object model matrix,
//! because wgpu's default `max_bind_groups` is 4 and groups 0–3 are
//! already taken.

use glam::{Mat4, Vec3};
use wgpu::util::DeviceExt;

use crate::bounds::Aabb;
use crate::error::RendererError;
use crate::gpu::GpuContext;
use crate::model::ModelUniform;
use crate::pipeline::{MaterialBinding, PBR_COMMON_SOURCE, Pipeline};

const SKINNED_VS_SOURCE: &str = include_str!("shaders/skinned_pbr_vs.wgsl");

/// The most joints one [`SkinnedMesh`]'s skeleton can have. Matches
/// `MAX_JOINTS` in `skinned_pbr_vs.wgsl`; the [`JointMatricesUniform`]
/// buffer is sized for exactly this many, and
/// [`JointMatricesUniform::from_matrices`] ignores (with a warning) any
/// beyond it.
pub const MAX_JOINTS: usize = 64;

/// A single vertex of a skinned mesh: the same position/normal/uv as
/// [`crate::Vertex`], plus up to four joint indices and their blend
/// weights.
///
/// `#[repr(C)]` + [`bytemuck::Pod`]/[`bytemuck::Zeroable`] make it safely
/// castable to bytes for GPU upload, the same pattern as
/// [`crate::Vertex`]. Joint indices are `u32` (not the `u16` glTF stores)
/// because WGSL has no 16-bit integer type — widen on import.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SkinnedVertex {
    /// Object-space position (bind pose).
    pub position: [f32; 3],
    /// Object-space normal (bind pose, unit length).
    pub normal: [f32; 3],
    /// Texture coordinate (`[u, v]`).
    pub uv: [f32; 2],
    /// Up to four joint indices into the mesh's skeleton. Unused slots
    /// should be `0` with a matching `0.0` weight.
    pub joints: [u32; 4],
    /// Blend weights for `joints`, expected to sum to ~1.
    pub weights: [f32; 4],
}

impl SkinnedVertex {
    const ATTRIBUTES: [wgpu::VertexAttribute; 5] = wgpu::vertex_attr_array![
        0 => Float32x3,
        1 => Float32x3,
        2 => Float32x2,
        3 => Uint32x4,
        4 => Float32x4,
    ];

    /// This vertex format's wgpu buffer layout, for a skinned pipeline's
    /// vertex state.
    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: size_of::<SkinnedVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBUTES,
        }
    }
}

/// A GPU-resident skinned mesh: vertex/index buffers, index count, and the
/// object-space (bind-pose) bounds of its geometry.
pub struct SkinnedMesh {
    /// Packed [`SkinnedVertex`] data.
    pub vertex_buffer: wgpu::Buffer,
    /// `u32` triangle-list indices into `vertex_buffer`.
    pub index_buffer: wgpu::Buffer,
    /// Number of indices to draw.
    pub index_count: u32,
    /// Tightest [`Aabb`] around the vertex positions, in the bind pose.
    /// Not used for culling yet — an animated pose can deform vertices
    /// outside it.
    pub local_bounds: Aabb,
}

/// Per-joint skinning matrices, packed for upload as a `@group(2)
/// @binding(1)` uniform.
///
/// `count` is the number of valid `matrices` entries; the rest are
/// identity padding. `#[repr(C)]` places `matrices` at byte offset 16
/// (after `count` plus `_padding`), matching WGSL's 16-byte alignment for
/// the `array<mat4x4<f32>>` member.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct JointMatricesUniform {
    /// Number of leading `matrices` entries that came from real joints.
    pub count: u32,
    _padding: [u32; 3],
    /// Column-major skinning matrix per joint; entries `count..` are
    /// identity.
    pub matrices: [[[f32; 4]; 4]; MAX_JOINTS],
}

const IDENTITY_MATRIX: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

impl JointMatricesUniform {
    /// All-identity, `count` zero — a skinned mesh bound with this renders
    /// in its bind pose.
    pub const IDENTITY: Self = Self {
        count: 0,
        _padding: [0; 3],
        matrices: [IDENTITY_MATRIX; MAX_JOINTS],
    };

    /// Packs `matrices` (e.g. from
    /// `engine_animation::compute_skinning_matrices`) for upload.
    ///
    /// A skeleton with more than [`MAX_JOINTS`] joints is truncated to the
    /// first [`MAX_JOINTS`] — logged once via `tracing::warn!`, not an
    /// error; the mesh just skins against the joints that fit.
    pub fn from_matrices(matrices: &[Mat4]) -> Self {
        let mut uniform = Self::IDENTITY;
        if matrices.len() > MAX_JOINTS {
            tracing::warn!(
                joints = matrices.len(),
                max = MAX_JOINTS,
                "skeleton exceeds MAX_JOINTS; extra joints ignored"
            );
        }
        let used = matrices.len().min(MAX_JOINTS);
        for (slot, matrix) in uniform.matrices[..used].iter_mut().zip(matrices) {
            *slot = matrix.to_cols_array_2d();
        }
        uniform.count = used as u32;
        uniform
    }
}

impl Default for JointMatricesUniform {
    /// [`JointMatricesUniform::IDENTITY`].
    fn default() -> Self {
        Self::IDENTITY
    }
}

/// A compiled skinning render pipeline. Its shader is
/// `pbr_common.wgsl` + `skinned_pbr_vs.wgsl` — identical lighting and
/// fragment stage to [`Pipeline`], only the vertex stage differs.
pub struct SkinnedPipeline {
    pub(crate) render_pipeline: wgpu::RenderPipeline,
    /// `@group(2)`: model matrix (`@binding(0)`) + joint matrices
    /// (`@binding(1)`).
    skin_bind_group_layout: wgpu::BindGroupLayout,
}

/// A skinned object's `@group(2)` resources: its model-matrix uniform
/// buffer, its joint-matrices uniform buffer, and the bind group exposing
/// both to a [`SkinnedPipeline`]'s shader.
///
/// Both buffers are `COPY_DST` — rewrite them per frame via
/// [`GpuContext::write_uniform_buffer`] (the model matrix from the
/// entity's transform, the joint matrices from a freshly sampled pose).
pub struct SkinnedBinding {
    /// Backs the `@binding(0)` [`ModelUniform`].
    pub model_buffer: wgpu::Buffer,
    /// Backs the `@binding(1)` [`JointMatricesUniform`].
    pub joints_buffer: wgpu::Buffer,
    pub(crate) bind_group: wgpu::BindGroup,
}

/// Everything needed to draw one skinned object in
/// [`GpuContext::render_scene`]: its `@group(2)` binding, its material,
/// and its geometry. Camera and lights are frame-shared, supplied
/// separately.
#[derive(Clone, Copy)]
pub struct SkinnedDrawable<'a> {
    /// This object's model + joint-matrix binding.
    pub skin: &'a SkinnedBinding,
    /// This object's material binding.
    pub material: &'a MaterialBinding,
    /// This object's skinned geometry.
    pub mesh: &'a SkinnedMesh,
}

impl GpuContext {
    /// Uploads `vertices`/`indices` as a new GPU-resident [`SkinnedMesh`].
    ///
    /// # Errors
    ///
    /// Returns [`RendererError::EmptyMesh`] if either slice is empty —
    /// same rationale as [`GpuContext::create_mesh`].
    pub fn create_skinned_mesh(
        &self,
        label: &str,
        vertices: &[SkinnedVertex],
        indices: &[u32],
    ) -> Result<SkinnedMesh, RendererError> {
        if vertices.is_empty() || indices.is_empty() {
            return Err(RendererError::EmptyMesh);
        }

        let local_bounds = Aabb::from_points(vertices.iter().map(|v| Vec3::from(v.position)))
            .unwrap_or(Aabb {
                min: Vec3::ZERO,
                max: Vec3::ZERO,
            });

        let vertex_buffer = self
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(&format!("{label} skinned vertices")),
                contents: bytemuck::cast_slice(vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let index_buffer = self
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(&format!("{label} skinned indices")),
                contents: bytemuck::cast_slice(indices),
                usage: wgpu::BufferUsages::INDEX,
            });

        Ok(SkinnedMesh {
            vertex_buffer,
            index_buffer,
            index_count: indices.len() as u32,
            local_bounds,
        })
    }

    /// Compiles the skinned PBR shader into a [`SkinnedPipeline`], reusing
    /// `base`'s camera/material/lights bind group layouts so an existing
    /// [`crate::CameraBinding`]/[`MaterialBinding`]/[`crate::LightsBinding`]
    /// work unmodified.
    pub fn create_skinned_pbr_pipeline(&self, base: &Pipeline, label: &str) -> SkinnedPipeline {
        let device = self.device();

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(&format!("{label} shader")),
            source: wgpu::ShaderSource::Wgsl(
                format!("{PBR_COMMON_SOURCE}\n{SKINNED_VS_SOURCE}").into(),
            ),
        });

        let skin_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("skinned model+joints bind group layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::VERTEX,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(&format!("{label} layout")),
            bind_group_layouts: &[
                Some(base.camera_layout()),
                Some(base.material_layout()),
                Some(&skin_bind_group_layout),
                Some(base.lights_layout()),
            ],
            immediate_size: 0,
        });

        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(SkinnedVertex::layout())],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(crate::pipeline::HDR_TEXTURE_FORMAT.into())],
            }),
            primitive: wgpu::PrimitiveState {
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            // Opaque skinned geometry — same depth rules as static meshes.
            depth_stencil: Some(crate::pipeline::opaque_depth_state()),
            // Matches the other scene-pass pipelines — see
            // `GpuContext::scene_multisample_state`.
            multisample: self.scene_multisample_state(),
            multiview_mask: None,
            cache: None,
        });

        SkinnedPipeline {
            render_pipeline,
            skin_bind_group_layout,
        }
    }

    /// Creates a [`SkinnedBinding`]: a model-matrix uniform buffer, a
    /// joint-matrices uniform buffer, and the bind group exposing both to
    /// `pipeline`'s shader at `@group(2)`.
    pub fn create_skinned_binding(
        &self,
        pipeline: &SkinnedPipeline,
        model: &ModelUniform,
        joints: &JointMatricesUniform,
    ) -> SkinnedBinding {
        let model_buffer = self.create_uniform_buffer("skinned model uniform", model);
        let joints_buffer = self.create_uniform_buffer("skinned joints uniform", joints);
        let bind_group = self.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("skinned model+joints bind group"),
            layout: &pipeline.skin_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: model_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: joints_buffer.as_entire_binding(),
                },
            ],
        });
        SkinnedBinding {
            model_buffer,
            joints_buffer,
            bind_group,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vertex_layout_stride_matches_struct_size() {
        assert_eq!(
            SkinnedVertex::layout().array_stride,
            size_of::<SkinnedVertex>() as wgpu::BufferAddress
        );
    }

    #[test]
    fn vertex_layout_has_five_attributes() {
        assert_eq!(SkinnedVertex::layout().attributes.len(), 5);
    }

    #[test]
    fn joint_matrices_uniform_places_matrices_at_offset_16() {
        assert_eq!(std::mem::offset_of!(JointMatricesUniform, matrices), 16);
    }

    #[test]
    fn joint_matrices_uniform_byte_size() {
        // 4 (count) + 12 (pad) + 64 * 64 (mat4 array) = 4112.
        assert_eq!(size_of::<JointMatricesUniform>(), 4112);
        assert_eq!(
            bytemuck::bytes_of(&JointMatricesUniform::IDENTITY).len(),
            4112
        );
    }

    #[test]
    fn identity_uniform_has_zero_count_and_identity_entries() {
        let uniform = JointMatricesUniform::IDENTITY;
        assert_eq!(uniform.count, 0);
        assert_eq!(uniform.matrices[0], IDENTITY_MATRIX);
        assert_eq!(uniform.matrices[MAX_JOINTS - 1], IDENTITY_MATRIX);
    }

    #[test]
    fn from_matrices_packs_column_major_and_sets_count() {
        let m = Mat4::from_translation(Vec3::new(1.0, 2.0, 3.0));
        let uniform = JointMatricesUniform::from_matrices(&[Mat4::IDENTITY, m]);
        assert_eq!(uniform.count, 2);
        assert_eq!(uniform.matrices[1], m.to_cols_array_2d());
        // Untouched slots stay identity.
        assert_eq!(uniform.matrices[2], IDENTITY_MATRIX);
    }

    #[test]
    fn from_matrices_truncates_past_max_joints() {
        let many = vec![Mat4::IDENTITY; MAX_JOINTS + 10];
        let uniform = JointMatricesUniform::from_matrices(&many);
        assert_eq!(uniform.count as usize, MAX_JOINTS);
    }

    #[test]
    fn from_empty_matrices_is_identity() {
        let uniform = JointMatricesUniform::from_matrices(&[]);
        assert_eq!(uniform.count, 0);
        assert_eq!(uniform.matrices[0], IDENTITY_MATRIX);
    }

    // The shader blends `sum(weight[i] * jointMatrix[joint[i]])` and
    // applies it to the position. Mirror that here so the weighting math
    // is covered without a GPU.
    fn skin_position(uniform: &JointMatricesUniform, vertex: &SkinnedVertex) -> Vec3 {
        let mut blended = Mat4::ZERO;
        for i in 0..4 {
            let joint = vertex.joints[i] as usize;
            let matrix = Mat4::from_cols_array_2d(&uniform.matrices[joint]);
            blended += matrix * vertex.weights[i];
        }
        blended.transform_point3(Vec3::from(vertex.position))
    }

    #[test]
    fn cpu_skin_blend_moves_a_fully_weighted_vertex_with_its_joint() {
        // Joint 1 translated +X by 5; a vertex fully weighted to joint 1
        // moves with it, a vertex fully weighted to joint 0 (identity)
        // stays put.
        let uniform = JointMatricesUniform::from_matrices(&[
            Mat4::IDENTITY,
            Mat4::from_translation(Vec3::new(5.0, 0.0, 0.0)),
        ]);

        let on_joint_1 = SkinnedVertex {
            position: [0.0, 1.0, 0.0],
            normal: [0.0, 1.0, 0.0],
            uv: [0.0, 0.0],
            joints: [1, 0, 0, 0],
            weights: [1.0, 0.0, 0.0, 0.0],
        };
        let on_joint_0 = SkinnedVertex {
            joints: [0, 0, 0, 0],
            weights: [1.0, 0.0, 0.0, 0.0],
            ..on_joint_1
        };

        assert_eq!(
            skin_position(&uniform, &on_joint_1),
            Vec3::new(5.0, 1.0, 0.0)
        );
        assert_eq!(
            skin_position(&uniform, &on_joint_0),
            Vec3::new(0.0, 1.0, 0.0)
        );
    }

    #[test]
    fn cpu_skin_blend_averages_two_half_weighted_joints() {
        let uniform = JointMatricesUniform::from_matrices(&[
            Mat4::IDENTITY,
            Mat4::from_translation(Vec3::new(4.0, 0.0, 0.0)),
        ]);
        let vertex = SkinnedVertex {
            position: [0.0, 0.0, 0.0],
            normal: [0.0, 0.0, 1.0],
            uv: [0.0, 0.0],
            joints: [0, 1, 0, 0],
            weights: [0.5, 0.5, 0.0, 0.0],
        };
        // Half of (0,0,0) and half of (4,0,0) -> (2,0,0).
        assert_eq!(skin_position(&uniform, &vertex), Vec3::new(2.0, 0.0, 0.0));
    }
}
