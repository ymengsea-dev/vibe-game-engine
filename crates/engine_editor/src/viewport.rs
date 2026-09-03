//! A 3D scene rendered off-screen into a texture, displayable inside an
//! egui panel (the "Scene view"/"Game view") via [`crate::EditorShell`].
//!
//! Deliberately minimal pipeline for this iteration — every entity draws
//! as the same plain lit cube, not the standalone game demo's full
//! pipeline (shadows, skybox, HDR, real per-entity meshes/materials,
//! physics, audio). [`Viewport::render`] takes one
//! `engine_utils::Transform` per entity (see
//! [`crate::EditorState::entity_transforms`]) and draws a cube at each —
//! real per-entity meshes are future work, but which *entities* show up
//! and where already matches the hierarchy/inspector panels exactly.
//!
//! [`Viewport::render`] can also draw the `gizmo` module's translate
//! handles at a given world position, in the same pass, using a second,
//! equally minimal line pipeline.

use engine_renderer::{Camera, DebugLineVertex, GpuContext, Mesh, ModelUniform, Vertex, cube};
use engine_utils::Transform;
use glam::{Mat4, Vec3};
use wgpu::util::DeviceExt;

use crate::error::EditorError;
use crate::gizmo::{self, Axis, GizmoMode};
use crate::shell::EditorShell;

const SHADER_SOURCE: &str = include_str!("shaders/viewport.wgsl");
const GIZMO_SHADER_SOURCE: &str = include_str!("shaders/gizmo.wgsl");

/// `egui_wgpu::Renderer::register_native_texture` requires exactly this
/// format for a texture it's asked to display.
const VIEWPORT_TEXTURE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Two [`DebugLineVertex`]s per axis handle ([`Axis::ALL`]) — the exact
/// size of the gizmo vertex buffer, written fresh (via
/// [`wgpu::Queue::write_buffer`], not recreated) every frame a gizmo is
/// drawn.
const GIZMO_VERTEX_COUNT: usize = Axis::ALL.len() * 2;

/// How many entities [`Viewport::render`] can draw at once — it pre-
/// allocates exactly this many (buffer, bind group) pairs up front
/// (see [`Viewport::new`]) so each entity gets its own model-uniform
/// buffer (required: they're all drawn within one render pass, so a
/// single shared, repeatedly-overwritten buffer would only ever show the
/// last entity written before the pass actually executes). Extra
/// entities beyond this cap are silently not drawn — a fixed limit,
/// same pragmatic choice as the viewport's fixed pixel size.
const MAX_VIEWPORT_ENTITIES: usize = 256;

/// [`DebugLineVertex`]'s field layout — `engine_renderer` doesn't expose
/// its own copy of this publicly (it's only needed inside that crate's
/// own pipeline setup), so this pipeline declares an identical one.
fn gizmo_vertex_layout() -> wgpu::VertexBufferLayout<'static> {
    const ATTRIBUTES: [wgpu::VertexAttribute; 2] =
        wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4];
    wgpu::VertexBufferLayout {
        array_stride: size_of::<DebugLineVertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &ATTRIBUTES,
    }
}

/// A fixed-size off-screen render of a placeholder 3D scene (one lit
/// cube), registered with egui as a displayable texture.
///
/// Fixed size rather than resizing to fill its panel — dynamically
/// resizing the viewport (recreating the texture and re-registering it
/// with egui every time its panel changes size) is future work.
pub struct Viewport {
    width: u32,
    height: u32,
    view: wgpu::TextureView,
    texture_id: egui::TextureId,
    render_pipeline: wgpu::RenderPipeline,
    camera_bind_group: wgpu::BindGroup,
    /// The camera uniform buffer behind `camera_bind_group`, kept so
    /// [`Viewport::set_dimension`] can re-upload a swapped camera.
    camera_buffer: wgpu::Buffer,
    /// One (buffer, bind group) pair per potential entity slot — see
    /// [`MAX_VIEWPORT_ENTITIES`].
    model_bindings: Vec<(wgpu::Buffer, wgpu::BindGroup)>,
    mesh: Mesh,
    camera: Camera,
    /// `true` while the 2D authoring camera / gizmo conventions are
    /// active — see [`Viewport::set_dimension`].
    two_d: bool,
    gizmo_pipeline: wgpu::RenderPipeline,
    gizmo_vertex_buffer: wgpu::Buffer,
}

impl Viewport {
    /// Creates a `width` by `height` viewport rendering a single cube lit
    /// by a fixed directional light, and registers its texture with
    /// `shell`'s egui renderer via [`EditorShell::register_texture`].
    ///
    /// # Errors
    ///
    /// Returns [`EditorError::Renderer`] if the cube's GPU mesh fails to
    /// build (in practice this never happens — [`cube`] always returns
    /// non-empty geometry — but mesh creation is fallible in general, so
    /// this stays a `Result` rather than assuming that never changes).
    pub fn new(
        gpu: &GpuContext,
        shell: &mut EditorShell,
        width: u32,
        height: u32,
    ) -> Result<Self, EditorError> {
        let device = gpu.device();

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("editor viewport target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: VIEWPORT_TEXTURE_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let texture_id = shell.register_texture(device, &view);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("viewport shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER_SOURCE.into()),
        });

        let camera_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("viewport camera bind group layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });
        let model_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("viewport model bind group layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("viewport pipeline layout"),
            bind_group_layouts: &[
                Some(&camera_bind_group_layout),
                Some(&model_bind_group_layout),
            ],
            immediate_size: 0,
        });

        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("viewport pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(Vertex::layout())],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(VIEWPORT_TEXTURE_FORMAT.into())],
            }),
            primitive: wgpu::PrimitiveState {
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let camera = Self::perspective_camera(width, height);
        let camera_buffer =
            gpu.create_uniform_buffer("viewport camera uniform", &camera.to_uniform());
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("viewport camera bind group"),
            layout: &camera_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });

        // One buffer/bind group per potential entity slot — see
        // `MAX_VIEWPORT_ENTITIES`'s docs for why each needs its own
        // rather than sharing one.
        let model_bindings: Vec<(wgpu::Buffer, wgpu::BindGroup)> = (0..MAX_VIEWPORT_ENTITIES)
            .map(|i| {
                let buffer = gpu.create_uniform_buffer(
                    &format!("viewport model uniform {i}"),
                    &ModelUniform::IDENTITY,
                );
                let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some(&format!("viewport model bind group {i}")),
                    layout: &model_bind_group_layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: buffer.as_entire_binding(),
                    }],
                });
                (buffer, bind_group)
            })
            .collect();

        let (vertices, indices) = cube();
        let mesh = gpu.create_mesh("viewport cube", &vertices, &indices)?;

        // Reuses `camera_bind_group_layout`/`camera_bind_group` above —
        // `gizmo.wgsl` reads the same leading `view_proj` field as
        // `viewport.wgsl`'s `CameraUniform`, the same trick
        // `engine_renderer`'s `debug_line.wgsl` uses against the main
        // game pipeline's camera.
        let gizmo_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("viewport gizmo shader"),
            source: wgpu::ShaderSource::Wgsl(GIZMO_SHADER_SOURCE.into()),
        });
        let gizmo_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("viewport gizmo pipeline layout"),
                bind_group_layouts: &[Some(&camera_bind_group_layout)],
                immediate_size: 0,
            });
        let gizmo_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("viewport gizmo pipeline"),
            layout: Some(&gizmo_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &gizmo_shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(gizmo_vertex_layout())],
            },
            fragment: Some(wgpu::FragmentState {
                module: &gizmo_shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(VIEWPORT_TEXTURE_FORMAT.into())],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let gizmo_vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("viewport gizmo vertex buffer"),
            contents: bytemuck::cast_slice(
                &[DebugLineVertex {
                    position: [0.0; 3],
                    color: [0.0; 4],
                }; GIZMO_VERTEX_COUNT],
            ),
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        });

        Ok(Self {
            width,
            height,
            view,
            texture_id,
            render_pipeline,
            camera_bind_group,
            camera_buffer,
            model_bindings,
            mesh,
            camera,
            two_d: false,
            gizmo_pipeline,
            gizmo_vertex_buffer,
        })
    }

    /// The perspective camera used in 3D authoring mode.
    fn perspective_camera(width: u32, height: u32) -> Camera {
        Camera::new(
            Vec3::new(1.5, 1.5, 2.5),
            Vec3::ZERO,
            width as f32 / height.max(1) as f32,
        )
    }

    /// The orthographic front-view camera used in 2D authoring mode:
    /// looking down `-Z` from `+Z`, `+Y` up, so world X runs right and
    /// world Y runs up on screen.
    fn orthographic_camera(width: u32, height: u32) -> Camera {
        Camera::new_orthographic(
            Vec3::new(0.0, 0.0, 10.0),
            Vec3::ZERO,
            width as f32 / height.max(1) as f32,
            6.0,
        )
    }

    /// Switches the viewport between 3D (perspective) and 2D
    /// (orthographic front view) authoring. Re-uploads the camera
    /// uniform only when the mode actually changes; also flips which
    /// gizmo axes [`Viewport::render`] draws (see
    /// [`crate::gizmo::axes_for`]).
    pub fn set_dimension(&mut self, gpu: &GpuContext, two_d: bool) {
        if self.two_d == two_d {
            return;
        }
        self.two_d = two_d;
        self.camera = if two_d {
            Self::orthographic_camera(self.width, self.height)
        } else {
            Self::perspective_camera(self.width, self.height)
        };
        gpu.write_uniform_buffer(&self.camera_buffer, &self.camera.to_uniform());
    }

    /// This viewport's size, in pixels — matches the texture
    /// [`Viewport::texture_id`] refers to.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The egui texture id this viewport's render target is registered
    /// under — pass to `egui::Image::new` to display it.
    pub fn texture_id(&self) -> egui::TextureId {
        self.texture_id
    }

    /// This viewport's camera's view-projection matrix — what
    /// `gizmo::project_to_screen` needs to place the gizmo's handles
    /// (see [`crate::EditorShell::run_frame`]'s Scene View panel, where
    /// that projection happens).
    pub fn view_projection_matrix(&self) -> Mat4 {
        self.camera.view_projection_matrix()
    }

    /// Renders `entities` (each drawn as this viewport's placeholder
    /// cube, at its own transform — see `MAX_VIEWPORT_ENTITIES`'s docs
    /// for the cap on how many) and, if `gizmo_origin` is `Some`, the
    /// gizmo's axis handles at that world position, into this viewport's
    /// off-screen texture. Which handles are drawn follows
    /// [`gizmo::axes_for`]`(gizmo_mode, two_d)` — all three in 3D, the
    /// screen-plane subset in 2D. Self-contained (its own command
    /// encoder and submit) — unlike the main game pipeline, this never
    /// touches a swapchain, so there's no acquire/present step to share
    /// with anything else.
    pub fn render(
        &self,
        gpu: &GpuContext,
        entities: &[Transform],
        gizmo_mode: GizmoMode,
        gizmo_origin: Option<Vec3>,
    ) {
        let mut encoder = gpu
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("editor viewport encoder"),
            });

        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("editor viewport pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.05,
                            g: 0.05,
                            b: 0.08,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            render_pass.set_pipeline(&self.render_pipeline);
            render_pass.set_bind_group(0, &self.camera_bind_group, &[]);
            render_pass.set_vertex_buffer(0, self.mesh.vertex_buffer.slice(..));
            render_pass
                .set_index_buffer(self.mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);

            // `zip` already stops at the shorter of the two — a no-op
            // cap once `entities.len() <= MAX_VIEWPORT_ENTITIES`, and the
            // actual cap (extra entities silently not drawn) otherwise.
            for (transform, (buffer, bind_group)) in entities.iter().zip(&self.model_bindings) {
                gpu.write_uniform_buffer(buffer, &ModelUniform::from(transform));
                render_pass.set_bind_group(1, bind_group, &[]);
                render_pass.draw_indexed(0..self.mesh.index_count, 0, 0..1);
            }

            if let Some(origin) = gizmo_origin {
                let axes = gizmo::axes_for(gizmo_mode, self.two_d);
                let mut vertices = [DebugLineVertex {
                    position: [0.0; 3],
                    color: [0.0; 4],
                }; GIZMO_VERTEX_COUNT];
                for (i, &axis) in axes.iter().enumerate() {
                    let (start, end) = gizmo::axis_endpoints(origin, axis);
                    let color = axis.color();
                    vertices[i * 2] = DebugLineVertex {
                        position: start.into(),
                        color,
                    };
                    vertices[i * 2 + 1] = DebugLineVertex {
                        position: end.into(),
                        color,
                    };
                }
                gpu.queue().write_buffer(
                    &self.gizmo_vertex_buffer,
                    0,
                    bytemuck::cast_slice(&vertices),
                );

                render_pass.set_pipeline(&self.gizmo_pipeline);
                render_pass.set_bind_group(0, &self.camera_bind_group, &[]);
                render_pass.set_vertex_buffer(0, self.gizmo_vertex_buffer.slice(..));
                render_pass.draw(0..(axes.len() * 2) as u32, 0..1);
            }
        }

        gpu.queue().submit(std::iter::once(encoder.finish()));
    }
}
