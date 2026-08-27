// Tonemap pass: full-screen triangle (same technique as skybox.wgsl) that
// reads the HDR offscreen target the main color pass rendered into, and
// compresses it into the swapchain's displayable [0,1] range with a
// filmic curve — a smooth rolloff for bright highlights instead of a
// harsh clip.

struct TonemapUniform {
    exposure: f32,
};

@group(0) @binding(0)
var hdr_texture: texture_2d<f32>;
@group(0) @binding(1)
var hdr_sampler: sampler;
@group(0) @binding(2)
var<uniform> tonemap: TonemapUniform;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOutput {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    let pos = positions[index];

    var out: VertexOutput;
    out.clip_position = vec4<f32>(pos, 1.0, 1.0);
    // NDC [-1,1] -> texture UV [0,1]; v flipped (texture v=0 is top, NDC
    // y=+1 is up), same convention `pbr.wgsl`'s shadow sampling uses.
    out.uv = vec2<f32>(pos.x * 0.5 + 0.5, pos.y * -0.5 + 0.5);
    return out;
}

// ACES filmic tonemap fit (Narkowicz 2015): maps unbounded HDR linear
// radiance into [0,1] with a filmic shoulder rather than a hard clip.
fn aces_tonemap(x: vec3<f32>) -> vec3<f32> {
    let a = 2.51;
    let b = 0.03;
    let c = 2.43;
    let d = 0.59;
    let e = 0.14;
    return clamp((x * (a * x + b)) / (x * (c * x + d) + e), vec3<f32>(0.0), vec3<f32>(1.0));
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let hdr_color = textureSample(hdr_texture, hdr_sampler, in.uv).rgb;
    let mapped = aces_tonemap(hdr_color * tonemap.exposure);
    // No manual gamma encode here: the swapchain's sRGB-format view
    // applies it automatically on write, the same convention `pbr.wgsl`
    // already relies on for its own linear-space output.
    return vec4<f32>(mapped, 1.0);
}
