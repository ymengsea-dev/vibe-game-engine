//! GPU instancing: draw N copies of one [`Mesh`] in a single
//! `draw_indexed(.., 0..N)` call, each copy placed by its own model matrix
//! from an instance-step vertex buffer.
//!
//! [`InstanceRaw`] is the per-instance GPU record (just a model matrix);
//! [`InstancedPipeline`] is `pbr_common.wgsl` + `instanced_pbr_vs.wgsl`
//! (model matrix from instance attributes, no `@group(2)` uniform);
//! [`InstancedDrawable`] bundles what one instanced draw needs.
//!
//! Reuses [`Pipeline`]'s camera (`@group(0)`), material (`@group(1)`), and
//! lights (`@group(3)`) bind group layouts unchanged. `@group(2)` is left
//! unbound — the model matrix is a vertex attribute here, not a uniform.

use engine_utils::Transform;
use wgpu::util::DeviceExt;

use crate::gpu::GpuContext;
use crate::mesh::{Mesh, Vertex};
use crate::pipeline::{MaterialBinding, PBR_COMMON_SOURCE, Pipeline};

const INSTANCED_VS_SOURCE: &str = include_str!("shaders/instanced_pbr_vs.wgsl");

/// One instance's GPU record: a column-major model matrix, consumed as
/// four `vec4` vertex attributes (locations 3..6) that advance once per
/// instance.
///
/// `#[repr(C)]` + [`bytemuck::Pod`]/[`bytemuck::Zeroable`] make it safely
/// castable to bytes for GPU upload, the same pattern as [`crate::Vertex`].
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct InstanceRaw {
    /// Column-major model matrix, matching WGSL's `mat4x4<f32>` layout.
    pub model: [[f32; 4]; 4],
}

impl InstanceRaw {
    const ATTRIBUTES: [wgpu::VertexAttribute; 4] = wgpu::vertex_attr_array![
        3 => Float32x4,
        4 => Float32x4,
        5 => Float32x4,
        6 => Float32x4,
    ];

    /// This record's wgpu buffer layout — `step_mode: Instance`, so each
    /// entry is read once per instance rather than once per vertex.
    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: size_of::<InstanceRaw>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &Self::ATTRIBUTES,
        }
    }
}

impl From<&Transform> for InstanceRaw {
    fn from(transform: &Transform) -> Self {
        Self {
            model: transform.to_matrix().to_cols_array_2d(),
        }
    }
}

impl From<Transform> for InstanceRaw {
    fn from(transform: Transform) -> Self {
        Self::from(&transform)
    }
}

/// A compiled instancing render pipeline. Its shader is
/// `pbr_common.wgsl` + `instanced_pbr_vs.wgsl` — identical lighting and
/// fragment stage to [`Pipeline`], only the vertex stage differs.
pub struct InstancedPipeline {
    pub(crate) render_pipeline: wgpu::RenderPipeline,
}

/// A GPU buffer of [`InstanceRaw`] records for one instanced draw. Thin
/// wrapper so callers (e.g. `engine_ecs`) can hold one without naming
/// `wgpu` — build it with [`GpuContext::create_instance_buffer`].
pub struct InstanceBuffer {
    pub(crate) buffer: wgpu::Buffer,
}

/// Everything one instanced draw in [`GpuContext::render_scene`] needs:
/// the shared material and geometry, plus the instance buffer and how
/// many instances it holds. Camera and lights are frame-shared, supplied
/// separately.
#[derive(Clone, Copy)]
pub struct InstancedDrawable<'a> {
    /// The material every instance shares.
    pub material: &'a MaterialBinding,
    /// The geometry every instance shares.
    pub mesh: &'a Mesh,
    /// Packed [`InstanceRaw`] records, one per instance.
    pub instance_buffer: &'a InstanceBuffer,
    /// Number of instances to draw (`instance_buffer` holds exactly this
    /// many `InstanceRaw`s).
    pub instance_count: u32,
}

impl GpuContext {
    /// Uploads `instances` as a `VERTEX`-usage [`InstanceBuffer`] for one
    /// instanced draw. Rebuild it whenever the instance set changes (a
    /// persistent, growable buffer is future work).
    pub fn create_instance_buffer(&self, label: &str, instances: &[InstanceRaw]) -> InstanceBuffer {
        let buffer = self
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: bytemuck::cast_slice(instances),
                usage: wgpu::BufferUsages::VERTEX,
            });
        InstanceBuffer { buffer }
    }

    /// Compiles the instanced PBR shader into an [`InstancedPipeline`],
    /// reusing `base`'s camera/material/lights bind group layouts.
    /// `@group(2)` is left unbound.
    pub fn create_instanced_pbr_pipeline(&self, base: &Pipeline, label: &str) -> InstancedPipeline {
        let device = self.device();

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(&format!("{label} shader")),
            source: wgpu::ShaderSource::Wgsl(
                format!("{PBR_COMMON_SOURCE}\n{INSTANCED_VS_SOURCE}").into(),
            ),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(&format!("{label} layout")),
            // `None` at index 2: the instanced vertex stage binds no
            // `@group(2)` (its model matrix is a vertex attribute).
            bind_group_layouts: &[
                Some(base.camera_layout()),
                Some(base.material_layout()),
                None,
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
                buffers: &[Some(Vertex::layout()), Some(InstanceRaw::layout())],
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
            depth_stencil: None,
            // Matches the other scene-pass pipelines — see
            // `GpuContext::scene_multisample_state`.
            multisample: self.scene_multisample_state(),
            multiview_mask: None,
            cache: None,
        });

        InstancedPipeline { render_pipeline }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::{Mat4, Vec3};

    #[test]
    fn instance_layout_stride_matches_struct_size() {
        assert_eq!(
            InstanceRaw::layout().array_stride,
            size_of::<InstanceRaw>() as wgpu::BufferAddress
        );
    }

    #[test]
    fn instance_layout_has_four_attributes_at_instance_step() {
        let layout = InstanceRaw::layout();
        assert_eq!(layout.attributes.len(), 4);
        assert_eq!(layout.step_mode, wgpu::VertexStepMode::Instance);
    }

    #[test]
    fn instance_layout_attribute_locations_are_3_through_6() {
        let locations: Vec<u32> = InstanceRaw::layout()
            .attributes
            .iter()
            .map(|a| a.shader_location)
            .collect();
        assert_eq!(locations, vec![3, 4, 5, 6]);
    }

    #[test]
    fn from_transform_matches_to_matrix_cols() {
        let transform = Transform::from_translation(Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(
            InstanceRaw::from(&transform).model,
            transform.to_matrix().to_cols_array_2d()
        );
    }

    #[test]
    fn from_owned_and_from_ref_agree() {
        let transform = Transform::from_translation(Vec3::new(4.0, 5.0, 6.0));
        assert_eq!(InstanceRaw::from(transform), InstanceRaw::from(&transform));
    }

    #[test]
    fn identity_transform_is_identity_matrix() {
        assert_eq!(
            InstanceRaw::from(Transform::IDENTITY).model,
            Mat4::IDENTITY.to_cols_array_2d()
        );
    }
}
