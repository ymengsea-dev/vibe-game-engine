//! Shader graph materials: render pipelines built from WGSL a shader graph
//! compiles to (see `engine_scene::shader_graph`), spliced into
//! `pbr_graph.wgsl`.
//!
//! Each distinct graph source is checked with naga, built once and cached
//! on its [`Pipeline`](crate::Pipeline); a source that fails the check is
//! remembered too, so a broken graph is reported once and its objects keep
//! the standard material. A graph material's parameters live in a uniform
//! buffer at `@group(1) @binding(7)` whose first slot carries the time.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use crate::gpu::GpuContext;
use crate::mesh::Vertex;
use crate::pipeline::{
    HDR_TEXTURE_FORMAT, MaterialBinding, PBR_SHARED_SOURCE, Pipeline, opaque_depth_state,
    transparent_depth_state,
};

/// Helpers graph code calls: value noise and a normalise that keeps zero
/// vectors at zero.
pub const GRAPH_HELPERS: &str = include_str!("shaders/graph_helpers.wgsl");
const PBR_GRAPH_SOURCE: &str = include_str!("shaders/pbr_graph.wgsl");

/// How many `vec4` slots a graph material's parameter buffer has: the time
/// in slot 0, then up to seven parameters.
pub const GRAPH_PARAM_SLOTS: usize = 8;

/// A shader graph for [`crate::MaterialMaps::graph`].
#[derive(Debug, Clone, Copy)]
pub struct GraphShader<'a> {
    /// The graph's `material_surface` and `material_vertex_offset`
    /// functions.
    pub source: &'a str,
    /// The parameters' starting values; slot 0 (the time) is overwritten
    /// every frame.
    pub values: [[f32; 4]; GRAPH_PARAM_SLOTS],
    /// The parameters' names, in slot order from slot 1.
    pub names: &'a [String],
}

/// A material binding's shader graph: its pipelines, its parameter buffer
/// (time in `values[0].x`, then the parameters) and the parameters' names.
pub(crate) struct GraphBinding {
    pub(crate) pipelines: Arc<GraphPipelines>,
    pub(crate) params: wgpu::Buffer,
    pub(crate) names: Vec<String>,
}

/// The parameter buffer's layout.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GraphParamsUniform {
    /// Time in `values[0][0]`, then the parameters.
    pub values: [[f32; 4]; GRAPH_PARAM_SLOTS],
}

impl GraphParamsUniform {
    /// A buffer holding `values`.
    pub fn new(values: [[f32; 4]; GRAPH_PARAM_SLOTS]) -> Self {
        Self { values }
    }
}

/// One graph's opaque, two-sided and transparent pipelines.
pub struct GraphPipelines {
    opaque: wgpu::RenderPipeline,
    two_sided: wgpu::RenderPipeline,
    transparent: wgpu::RenderPipeline,
}

impl GraphPipelines {
    pub(crate) fn opaque(&self, double_sided: bool) -> &wgpu::RenderPipeline {
        if double_sided {
            &self.two_sided
        } else {
            &self.opaque
        }
    }

    pub(crate) fn transparent(&self) -> &wgpu::RenderPipeline {
        &self.transparent
    }
}

/// The graph pipelines a [`Pipeline`] has built, keyed by a hash of their
/// source (`None` for sources that failed the check), plus the layout
/// they share with the standard pipeline and the shader clock.
pub(crate) struct GraphCache {
    layout: wgpu::PipelineLayout,
    built: Mutex<HashMap<u64, Option<Arc<GraphPipelines>>>>,
    /// Bound at binding 7 by materials that are not graphs.
    default_params: wgpu::Buffer,
    /// Seconds, as `f32` bits.
    time: AtomicU32,
}

impl GraphCache {
    pub(crate) fn new(gpu: &GpuContext, layout: wgpu::PipelineLayout) -> Self {
        Self {
            layout,
            built: Mutex::new(HashMap::new()),
            default_params: gpu.create_uniform_buffer(
                "default shader graph parameters",
                &GraphParamsUniform::new([[0.0; 4]; GRAPH_PARAM_SLOTS]),
            ),
            time: AtomicU32::new(0),
        }
    }

    pub(crate) fn default_params(&self) -> &wgpu::Buffer {
        &self.default_params
    }

    /// The time slot as the parameter buffer stores it.
    pub(crate) fn time(&self) -> [f32; 4] {
        [
            f32::from_bits(self.time.load(Ordering::Relaxed)),
            0.0,
            0.0,
            0.0,
        ]
    }
}

/// The whole module for a graph's `source`.
pub fn graph_module_source(source: &str) -> String {
    format!("{PBR_SHARED_SOURCE}\n{PBR_GRAPH_SOURCE}\n{GRAPH_HELPERS}\n{source}")
}

/// Checks a graph's `source` the way wgpu would, without a GPU.
///
/// # Errors
///
/// The parse or validation error, as text.
pub fn check_graph_source(source: &str) -> Result<(), String> {
    let module = naga::front::wgsl::parse_str(&graph_module_source(source))
        .map_err(|error| error.emit_to_string(source))?;
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::empty(),
    )
    .validate(&module)
    .map_err(|error| format!("{error:?}"))?;
    Ok(())
}

impl Pipeline {
    /// Sets the time shader graphs see, in seconds. The editor and the
    /// player call this once per frame.
    pub fn set_shader_time(&self, seconds: f32) {
        self.graphs.time.store(seconds.to_bits(), Ordering::Relaxed);
    }
}

impl GpuContext {
    /// The pipelines for a graph's `source`, built and cached on first use.
    /// `None` (with a warning, once per source) when the source fails the
    /// naga check, so callers fall back to the standard material.
    pub fn graph_pipelines(
        &self,
        pipeline: &Pipeline,
        source: &str,
    ) -> Option<Arc<GraphPipelines>> {
        let mut hasher = DefaultHasher::new();
        source.hash(&mut hasher);
        let key = hasher.finish();
        let mut built = pipeline
            .graphs
            .built
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(entry) = built.get(&key) {
            return entry.clone();
        }
        let entry = match check_graph_source(source) {
            Ok(()) => Some(Arc::new(self.build_graph_pipelines(pipeline, source))),
            Err(error) => {
                tracing::warn!(
                    "shader graph failed to build; using the standard material: {error}"
                );
                None
            }
        };
        built.insert(key, entry.clone());
        entry
    }

    fn build_graph_pipelines(&self, pipeline: &Pipeline, source: &str) -> GraphPipelines {
        let device = self.device();
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shader graph"),
            source: wgpu::ShaderSource::Wgsl(graph_module_source(source).into()),
        });
        let layout = &pipeline.graphs.layout;
        let build = |label: &str, cull_mode, blend, depth| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(layout),
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
                    targets: &[Some(wgpu::ColorTargetState {
                        format: HDR_TEXTURE_FORMAT,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    cull_mode,
                    ..Default::default()
                },
                depth_stencil: Some(depth),
                multisample: self.scene_multisample_state(),
                multiview_mask: None,
                cache: None,
            })
        };
        GraphPipelines {
            opaque: build(
                "shader graph",
                Some(wgpu::Face::Back),
                None,
                opaque_depth_state(),
            ),
            two_sided: build("shader graph (two-sided)", None, None, opaque_depth_state()),
            transparent: build(
                "shader graph (transparent)",
                None,
                Some(wgpu::BlendState::ALPHA_BLENDING),
                transparent_depth_state(),
            ),
        }
    }

    /// Sets shader graph parameter `name` of `material` to `value`.
    /// Returns `false` when the material is not a graph or has no such
    /// parameter.
    pub fn set_graph_parameter(
        &self,
        material: &MaterialBinding,
        name: &str,
        value: [f32; 4],
    ) -> bool {
        let Some(graph) = &material.graph else {
            return false;
        };
        let Some(index) = graph.names.iter().position(|other| other == name) else {
            return false;
        };
        if index + 1 >= GRAPH_PARAM_SLOTS {
            return false;
        }
        let offset = ((index + 1) * std::mem::size_of::<[f32; 4]>()) as u64;
        self.queue()
            .write_buffer(&graph.params, offset, bytemuck::cast_slice(&value));
        true
    }
}

impl MaterialBinding {
    /// Whether this material draws with a shader graph.
    pub fn is_graph(&self) -> bool {
        self.graph.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLAT: &str = "
fn material_surface(input: SurfaceInput) -> SurfaceOutput {
    var out: SurfaceOutput;
    out.base_color = vec3<f32>(graph_noise(input.world_position));
    out.metallic = 0.0;
    out.roughness = 0.5;
    out.normal = textureSample(normal_map, base_color_sampler, input.uv).xyz;
    out.emissive = vec3<f32>(sin(input.time));
    out.alpha = graph_params.values[1].x;
    return out;
}

fn material_vertex_offset(input: SurfaceInput) -> vec3<f32> {
    return input.normal * textureSampleLevel(base_color_texture, base_color_sampler, input.uv, 0.0).x;
}
";

    #[test]
    fn the_graph_template_validates_with_a_surface() {
        check_graph_source(FLAT).expect("valid");
    }

    #[test]
    fn a_broken_graph_is_refused_with_its_error() {
        let error = check_graph_source("fn material_surface() {}").expect_err("broken");
        assert!(!error.is_empty());
    }
}
