// Depth-only shader: renders a scene from a directional light's point of
// view into a shadow map. The fragment stage discards transparent texels
// from alpha-masked materials before depth is written.

struct ShadowUniform {
    light_space_matrix: mat4x4<f32>,
};

@group(0) @binding(0)
var<uniform> shadow: ShadowUniform;

struct ModelUniform {
    model: mat4x4<f32>,
    // `x` is the distance fade; see `ModelUniform::params`.
    params: vec4<f32>,
};

@group(1) @binding(0)
var base_color_texture: texture_2d<f32>;
@group(1) @binding(1)
var base_color_sampler: sampler;

struct MaterialUniform {
    base_color_factor: vec4<f32>,
    metallic_factor: f32,
    roughness_factor: f32,
    alpha_cutoff: f32,
    alpha_mode: u32,
    emissive_factor: vec3<f32>,
    normal_scale: f32,
    occlusion_strength: f32,
    uv_transform: vec4<f32>,
};

@group(1) @binding(2)
var<uniform> material: MaterialUniform;

@group(2) @binding(0)
var<uniform> object: ModelUniform;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) @interpolate(flat) fade: f32,
};

// Same 4x4 ordered dither as `PBR_COMMON_SOURCE`, so a fading object's
// shadow thins out with it.
fn dither_threshold(position: vec2<f32>) -> f32 {
    let bayer = array<f32, 16>(
        0.0, 8.0, 2.0, 10.0,
        12.0, 4.0, 14.0, 6.0,
        3.0, 11.0, 1.0, 9.0,
        15.0, 7.0, 13.0, 5.0,
    );
    let cell = vec2<u32>(position) % vec2<u32>(4u);
    return (bayer[cell.y * 4u + cell.x] + 0.5) / 16.0;
}

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    let world_position = object.model * vec4<f32>(in.position, 1.0);
    return VertexOutput(shadow.light_space_matrix * world_position, in.uv, object.params.x);
}

@fragment
fn fs_main(in: VertexOutput) {
    if in.fade < dither_threshold(in.position.xy) {
        discard;
    }
    let base_color = textureSample(
        base_color_texture,
        base_color_sampler,
        in.uv * material.uv_transform.xy + material.uv_transform.zw,
    )
        * material.base_color_factor;
    if material.alpha_mode == 1u && base_color.a < material.alpha_cutoff {
        discard;
    }
}
