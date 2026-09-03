// Instanced vertex stage for the PBR shader. Concatenated onto
// `pbr_common.wgsl` (which declares `camera` and `VertexOutput`) by
// `GpuContext::create_instanced_pbr_pipeline`.
//
// The per-object model matrix comes from an instance-step vertex buffer
// (`InstanceRaw`, one entry per instance) instead of a `@group(2)`
// uniform — so N copies of a mesh draw in one `draw_indexed(.., 0..N)`.
// Locations 0..2 are the shared `Vertex` attributes; 3..6 are the model
// matrix's four columns, advancing once per instance.

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) model_col0: vec4<f32>,
    @location(4) model_col1: vec4<f32>,
    @location(5) model_col2: vec4<f32>,
    @location(6) model_col3: vec4<f32>,
};

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;

    let model = mat4x4<f32>(in.model_col0, in.model_col1, in.model_col2, in.model_col3);

    let world_position = model * vec4<f32>(in.position, 1.0);
    out.clip_position = camera.view_proj * world_position;
    out.world_position = world_position.xyz;
    // Same simplification as the other vertex stages: no dedicated normal
    // matrix, so non-uniform instance scale would skew normals.
    out.world_normal = (model * vec4<f32>(in.normal, 0.0)).xyz;
    out.uv = in.uv;
    return out;
}
