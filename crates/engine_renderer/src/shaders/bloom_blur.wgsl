// Bloom blur: one axis of a separable 9-tap Gaussian (centre + four
// symmetric pairs). `direction` is a single texel step along the axis
// being blurred this invocation — (texel_x, 0) for the horizontal pass,
// (0, texel_y) for the vertical one — so the same shader does both.
// Ping-ponged between the two half-resolution bloom targets; a handful of
// H/V iterations widen the kernel well past its literal 9-tap footprint.

struct BlurUniform {
    direction: vec2<f32>,
    _padding: vec2<f32>,
};

@group(0) @binding(0)
var source_texture: texture_2d<f32>;
@group(0) @binding(1)
var source_sampler: sampler;
@group(0) @binding(2)
var<uniform> blur: BlurUniform;

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

// Centre weight then the four one-sided weights, matching
// `post::GAUSSIAN_9TAP_WEIGHTS`. Centre + 2 x (sum of the rest) = 1.0.
const W0: f32 = 0.227027;
const W1: f32 = 0.1945946;
const W2: f32 = 0.1216216;
const W3: f32 = 0.0540541;
const W4: f32 = 0.0162162;

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let d = blur.direction;
    var result = textureSample(source_texture, source_sampler, in.uv).rgb * W0;
    result += textureSample(source_texture, source_sampler, in.uv + d * 1.0).rgb * W1;
    result += textureSample(source_texture, source_sampler, in.uv - d * 1.0).rgb * W1;
    result += textureSample(source_texture, source_sampler, in.uv + d * 2.0).rgb * W2;
    result += textureSample(source_texture, source_sampler, in.uv - d * 2.0).rgb * W2;
    result += textureSample(source_texture, source_sampler, in.uv + d * 3.0).rgb * W3;
    result += textureSample(source_texture, source_sampler, in.uv - d * 3.0).rgb * W3;
    result += textureSample(source_texture, source_sampler, in.uv + d * 4.0).rgb * W4;
    result += textureSample(source_texture, source_sampler, in.uv - d * 4.0).rgb * W4;
    return vec4<f32>(result, 1.0);
}
