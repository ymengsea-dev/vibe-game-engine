// Minimal lit-cube shader for the editor's "Scene view" placeholder
// content (see `crate::Viewport`) — deliberately simple (flat Lambertian
// diffuse, one fixed light, no shadows/PBR/material system) since this
// feature is about proving render-to-texture-then-display-in-egui works,
// not about matching the standalone game's full pipeline.

struct CameraUniform {
    view_proj: mat4x4<f32>,
    // xyz = world-space eye position; w unused. Matches
    // `engine_renderer::CameraUniform`'s layout exactly (this shader
    // binds a buffer built from that same type).
    view_position: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> camera: CameraUniform;

struct ModelUniform {
    model: mat4x4<f32>,
};

@group(1) @binding(0)
var<uniform> object: ModelUniform;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_normal: vec3<f32>,
};

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    let world_position = object.model * vec4<f32>(in.position, 1.0);
    out.clip_position = camera.view_proj * world_position;
    out.world_normal = (object.model * vec4<f32>(in.normal, 0.0)).xyz;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let n = normalize(in.world_normal);
    let to_light = normalize(vec3<f32>(0.4, 1.0, 0.3));
    let n_dot_l = max(dot(n, to_light), 0.0);

    let base_color = vec3<f32>(0.55, 0.7, 1.0);
    let color = base_color * (0.25 + 0.75 * n_dot_l);

    // Manual gamma encode: this target is `Rgba8Unorm` (no hardware sRGB
    // auto-conversion), unlike the main PBR pipeline's Srgb swapchain —
    // egui-wgpu requires exactly this format for a registered viewport
    // texture, so the encode has to happen here instead.
    let encoded = pow(color, vec3<f32>(1.0 / 2.2));
    return vec4<f32>(encoded, 1.0);
}
