// Depth-only vertex shader: renders a scene from a directional light's
// point of view into a shadow map. No fragment shader — the render
// pipeline writes only the depth attachment.

struct ShadowUniform {
    light_space_matrix: mat4x4<f32>,
};

@group(0) @binding(0)
var<uniform> shadow: ShadowUniform;

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

@vertex
fn vs_main(in: VertexInput) -> @builtin(position) vec4<f32> {
    let world_position = object.model * vec4<f32>(in.position, 1.0);
    return shadow.light_space_matrix * world_position;
}
