// Unskinned vertex stage for the PBR shader. Concatenated onto
// `pbr_common.wgsl` (which declares `camera` and `VertexOutput`) by
// `GpuContext::create_pbr_pipeline`.

struct ModelUniform {
    model: mat4x4<f32>,
};

@group(2) @binding(0)
var<uniform> object: ModelUniform;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
};

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    let world_position = object.model * vec4<f32>(in.position, 1.0);
    out.clip_position = camera.view_proj * world_position;
    out.world_position = world_position.xyz;
    // Normals ignore translation/scale by design here (no normal matrix
    // yet — a concern once non-uniform scale matters).
    out.world_normal = (object.model * vec4<f32>(in.normal, 0.0)).xyz;
    out.uv = in.uv;
    return out;
}
