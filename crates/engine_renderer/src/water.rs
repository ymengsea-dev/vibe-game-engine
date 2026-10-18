//! GPU-neutral data for water surface rendering.

use bytemuck::Zeroable;
use glam::{Vec2, Vec3};

use crate::{GpuContext, Mesh, ModelBinding, ModelUniform, Pipeline, Vertex};

const WATER_SHADER: &str = include_str!("shaders/water.wgsl");

/// Maximum transient ripple/splash impulses stored for one water surface.
pub const MAX_WATER_IMPULSES: usize = 64;
const WATER_GRID_SUBDIVISIONS: u32 = 64;

/// One transient water disturbance.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WaterImpulse {
    /// World-space centre.
    pub position: Vec3,
    /// Radius in world units.
    pub radius: f32,
    /// Visual strength.
    pub strength: f32,
    /// Seconds since creation.
    pub age: f32,
}

impl WaterImpulse {
    /// Creates one finite, non-negative impulse.
    pub fn new(position: Vec3, radius: f32, strength: f32, age: f32) -> Self {
        Self {
            position: if position.is_finite() {
                position
            } else {
                Vec3::ZERO
            },
            radius: radius.max(0.0),
            strength: strength.max(0.0),
            age: age.max(0.0),
        }
    }
}

/// Fixed-capacity impulses for one surface. Overflow removes the oldest.
#[derive(Debug, Clone)]
pub struct WaterImpulseQueue {
    surface_id: u64,
    impulses: Vec<WaterImpulse>,
}

impl WaterImpulseQueue {
    /// Creates an empty queue for one stable surface identity.
    pub fn new(surface_id: u64) -> Self {
        Self {
            surface_id,
            impulses: Vec::with_capacity(MAX_WATER_IMPULSES),
        }
    }

    /// Stable identity of the surface these impulses target.
    pub fn surface_id(&self) -> u64 {
        self.surface_id
    }

    /// Appends `impulse`, discarding the oldest one if full.
    pub fn push(&mut self, impulse: WaterImpulse) {
        if self.impulses.len() == MAX_WATER_IMPULSES {
            self.impulses.remove(0);
        }
        self.impulses.push(impulse);
    }

    /// Number of retained impulses.
    pub fn len(&self) -> usize {
        self.impulses.len()
    }

    /// Whether this surface has no active impulses.
    pub fn is_empty(&self) -> bool {
        self.impulses.is_empty()
    }

    /// Retained impulses from oldest to newest.
    pub fn as_slice(&self) -> &[WaterImpulse] {
        &self.impulses
    }

    /// Advances impulse ages and discards entries at or beyond `lifetime`.
    pub fn advance(&mut self, delta: f32, lifetime: f32) {
        if !delta.is_finite() || delta <= 0.0 || !lifetime.is_finite() || lifetime <= 0.0 {
            return;
        }
        for impulse in &mut self.impulses {
            impulse.age += delta;
        }
        self.impulses.retain(|impulse| impulse.age < lifetime);
    }
}

/// CPU representation of one water surface shader uniform.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WaterUniform {
    center: Vec3,
    base_height: f32,
    extent: Vec2,
    wave_direction: Vec2,
    amplitude: f32,
    length: f32,
    speed: f32,
}

impl WaterUniform {
    /// Normalizes wave direction and clamps shader-unsafe scalar inputs.
    pub fn new(
        center: Vec3,
        extent: Vec2,
        wave_direction: Vec2,
        amplitude: f32,
        length: f32,
        speed: f32,
    ) -> Self {
        Self {
            center: if center.is_finite() {
                center
            } else {
                Vec3::ZERO
            },
            base_height: if center.y.is_finite() { center.y } else { 0.0 },
            extent: Vec2::new(extent.x.max(0.0), extent.y.max(0.0)),
            wave_direction: wave_direction.normalize_or_zero(),
            amplitude: amplitude.max(0.0),
            length: length.max(f32::EPSILON),
            speed: if speed.is_finite() { speed } else { 0.0 },
        }
    }

    /// Still-water height.
    pub fn base_height(self) -> f32 {
        self.base_height
    }
    /// Surface half extents.
    pub fn extent(self) -> Vec2 {
        self.extent
    }
    /// Normalized XZ wave heading.
    pub fn wave_direction(self) -> Vec2 {
        self.wave_direction
    }
}

/// GPU layout for one water surface. Kept separate from [`WaterUniform`]
/// so CPU validation never depends on shader padding rules.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct WaterGpuUniform {
    /// Surface centre XYZ and still-water height.
    pub center_height: [f32; 4],
    /// Half extents and wave amplitude/length.
    pub extent_wave: [f32; 4],
    /// Wave direction, speed, and animation time.
    pub direction_time: [f32; 4],
    /// Near-shore colour.
    pub shallow: [f32; 4],
    /// Deep-water colour.
    pub deep: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct WaterGpuImpulse {
    position_radius: [f32; 4],
    strength_age: [f32; 4],
}

impl From<WaterImpulse> for WaterGpuImpulse {
    fn from(impulse: WaterImpulse) -> Self {
        Self {
            position_radius: [
                impulse.position.x,
                impulse.position.y,
                impulse.position.z,
                impulse.radius,
            ],
            strength_age: [impulse.strength, impulse.age, 0.0, 0.0],
        }
    }
}

impl WaterGpuUniform {
    /// Packs one safe surface uniform for shader upload.
    pub fn new(
        center: Vec3,
        extent: Vec2,
        direction: Vec2,
        amplitude: f32,
        length: f32,
        speed: f32,
        time: f32,
    ) -> Self {
        let center = if center.is_finite() {
            center
        } else {
            Vec3::ZERO
        };
        let extent = if extent.is_finite() {
            extent.max(Vec2::ZERO)
        } else {
            Vec2::ZERO
        };
        let direction = if direction.is_finite() {
            direction.normalize_or_zero()
        } else {
            Vec2::ZERO
        };
        Self {
            center_height: [center.x, center.y, center.z, center.y],
            extent_wave: [
                extent.x,
                finite_non_negative(amplitude),
                finite_positive(length),
                extent.y,
            ],
            direction_time: [
                direction.x,
                direction.y,
                finite_or_zero(speed),
                finite_or_zero(time),
            ],
            shallow: [0.08, 0.38, 0.40, 1.0],
            deep: [0.01, 0.08, 0.20, 1.0],
        }
    }
}

fn finite_or_zero(value: f32) -> f32 {
    if value.is_finite() { value } else { 0.0 }
}

fn finite_non_negative(value: f32) -> f32 {
    finite_or_zero(value).max(0.0)
}

fn finite_positive(value: f32) -> f32 {
    finite_or_zero(value).max(f32::EPSILON)
}

/// Compiled dedicated water pipeline plus one shared surface grid.
pub struct WaterPipeline {
    pub(crate) pipeline: wgpu::RenderPipeline,
    pub(crate) mesh: Mesh,
    layout: wgpu::BindGroupLayout,
}
/// Persistent GPU bindings for one water surface.
pub struct WaterBinding {
    /// Per-surface wave and colour settings.
    pub uniform: wgpu::Buffer,
    impulses: wgpu::Buffer,
    pub(crate) bind_group: wgpu::BindGroup,
    /// Surface world transform for the shared grid mesh.
    pub model: ModelBinding,
}
/// One surface draw in a [`WaterFrame`].
pub struct WaterDraw<'a> {
    /// Persistent bindings for this surface.
    pub binding: &'a WaterBinding,
}
/// Water surfaces to draw in the opaque scene pass.
pub struct WaterFrame<'a> {
    /// Shared water pipeline and grid mesh.
    pub pipeline: &'a WaterPipeline,
    /// Surface draws for this frame.
    pub draws: &'a [WaterDraw<'a>],
}

impl GpuContext {
    /// Creates the reusable water shader pipeline and a two-triangle unit grid.
    pub fn create_water_pipeline(
        &self,
        base: &Pipeline,
        label: &str,
    ) -> Result<WaterPipeline, crate::RendererError> {
        let shader = self
            .device()
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(WATER_SHADER.into()),
            });
        let layout = self
            .device()
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("water uniform layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });
        let pipeline_layout =
            self.device()
                .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("water layout"),
                    bind_group_layouts: &[
                        Some(base.camera_layout()),
                        Some(&layout),
                        Some(base.model_layout()),
                    ],
                    immediate_size: 0,
                });
        let pipeline = self
            .device()
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[Some(Vertex::layout())],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    targets: &[Some(crate::pipeline::HDR_TEXTURE_FORMAT.into())],
                }),
                primitive: wgpu::PrimitiveState {
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: Some(crate::pipeline::opaque_depth_state()),
                multisample: self.scene_multisample_state(),
                multiview_mask: None,
                cache: None,
            });
        let (vertices, indices) = water_grid(WATER_GRID_SUBDIVISIONS);
        let mesh = self.create_mesh("water grid", &vertices, &indices)?;
        Ok(WaterPipeline {
            pipeline,
            mesh,
            layout,
        })
    }
    /// Allocates one persistent surface binding.
    pub fn create_water_binding(
        &self,
        pipeline: &WaterPipeline,
        base: &Pipeline,
        uniform: &WaterGpuUniform,
    ) -> WaterBinding {
        let buffer = self.create_uniform_buffer("water uniform", uniform);
        let impulses = [WaterGpuImpulse::zeroed(); MAX_WATER_IMPULSES];
        let impulse_buffer = self.create_uniform_buffer("water impulses", &impulses);
        let bind_group = self.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("water bind group"),
            layout: &pipeline.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: impulse_buffer.as_entire_binding(),
                },
            ],
        });
        WaterBinding {
            uniform: buffer,
            impulses: impulse_buffer,
            bind_group,
            model: self.create_model_binding(base, &ModelUniform::IDENTITY),
        }
    }

    /// Updates an existing surface without allocating new GPU resources.
    pub fn write_water_binding(
        &self,
        binding: &WaterBinding,
        uniform: &WaterGpuUniform,
        impulses: &[WaterImpulse],
    ) {
        self.write_uniform_buffer(&binding.uniform, uniform);
        let mut gpu_impulses = [WaterGpuImpulse::zeroed(); MAX_WATER_IMPULSES];
        for (target, impulse) in gpu_impulses.iter_mut().zip(impulses.iter()) {
            *target = (*impulse).into();
        }
        self.write_uniform_buffer(&binding.impulses, &gpu_impulses);
    }
}

fn water_grid(subdivisions: u32) -> (Vec<Vertex>, Vec<u32>) {
    let side = subdivisions + 1;
    let mut vertices = Vec::with_capacity((side * side) as usize);
    let mut indices = Vec::with_capacity((subdivisions * subdivisions * 6) as usize);
    for z in 0..=subdivisions {
        for x in 0..=subdivisions {
            let uv = Vec2::new(x as f32, z as f32) / subdivisions as f32;
            vertices.push(Vertex {
                position: [uv.x * 2.0 - 1.0, 0.0, uv.y * 2.0 - 1.0],
                normal: [0.0, 1.0, 0.0],
                uv: uv.to_array(),
            });
        }
    }
    for z in 0..subdivisions {
        for x in 0..subdivisions {
            let top_left = z * side + x;
            indices.extend_from_slice(&[
                top_left,
                top_left + 1,
                top_left + side + 1,
                top_left,
                top_left + side + 1,
                top_left + side,
            ]);
        }
    }
    (vertices, indices)
}

#[cfg(test)]
mod tests {
    use super::water_grid;

    #[test]
    fn grid_has_two_triangles_per_cell() {
        let (vertices, indices) = water_grid(2);
        assert_eq!(vertices.len(), 9);
        assert_eq!(indices.len(), 24);
        assert_eq!(vertices[0].position, [-1.0, 0.0, -1.0]);
        assert_eq!(vertices[8].position, [1.0, 0.0, 1.0]);
    }
}
