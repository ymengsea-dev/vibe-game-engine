// Bloom bright-pass: full-screen triangle (same technique as tonemap.wgsl)
// that reads the HDR scene target and keeps only the portion of each pixel
// above a brightness threshold, with a soft "knee" so pixels near the
// threshold fade in smoothly instead of popping. Rendered into a
// half-resolution target — the linear sampler averaging four source texels
// per fetch doubles as a cheap box downsample.

struct PrefilterUniform {
    threshold: f32,
    soft_knee: f32,
    _padding: vec2<f32>,
};

@group(0) @binding(0)
var hdr_texture: texture_2d<f32>;
@group(0) @binding(1)
var hdr_sampler: sampler;
@group(0) @binding(2)
var<uniform> prefilter: PrefilterUniform;

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
    out.uv = vec2<f32>(pos.x * 0.5 + 0.5, pos.y * -0.5 + 0.5);
    return out;
}

// Soft-knee bright-pass response. `brightness` is the pixel's max channel;
// the return value is the scalar fraction of the colour that survives.
// Mirrors `post::soft_knee_response` on the CPU side (kept in sync so that
// function's unit tests describe this shader's behaviour).
fn soft_knee_response(brightness: f32, threshold: f32, soft_knee: f32) -> f32 {
    let knee = max(soft_knee, 1e-4);
    var soft = clamp(brightness - threshold + knee, 0.0, 2.0 * knee);
    soft = soft * soft / (4.0 * knee);
    return max(max(soft, brightness - threshold), 0.0) / max(brightness, 1e-4);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let color = textureSample(hdr_texture, hdr_sampler, in.uv).rgb;
    let brightness = max(color.r, max(color.g, color.b));
    let contribution = soft_knee_response(brightness, prefilter.threshold, prefilter.soft_knee);
    return vec4<f32>(color * contribution, 1.0);
}
