// Final post-processing pass: full-screen triangle that takes the HDR
// scene target and the blurred bloom target and produces the displayable
// swapchain image. Supersedes the old standalone tonemap pass — it folds
// bloom compositing, colour grading, a screen-space toon outline, and the
// same ACES filmic tonemap into one pass so the swapchain-sized image is
// only written once.
//
// Order of operations (all in linear HDR until the tonemap step):
//   scene + bloom -> exposure -> white balance -> colour filter ->
//   contrast -> saturation -> ACES tonemap -> outline mix (in LDR).

struct CompositeUniform {
    exposure: f32,
    bloom_intensity: f32,
    grade_temperature: f32,
    grade_tint: f32,
    grade_contrast: f32,
    grade_saturation: f32,
    outline_threshold: f32,
    outline_thickness: f32,
    grade_color_filter: vec3<f32>,
    outline_intensity: f32,
    outline_color: vec3<f32>,
    _padding0: f32,
    texel_size: vec2<f32>,
    _padding1: vec2<f32>,
};

@group(0) @binding(0)
var hdr_texture: texture_2d<f32>;
@group(0) @binding(1)
var linear_sampler: sampler;
@group(0) @binding(2)
var bloom_texture: texture_2d<f32>;
@group(0) @binding(3)
var<uniform> post: CompositeUniform;

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

const LUMA_WEIGHTS: vec3<f32> = vec3<f32>(0.2126, 0.7152, 0.0722);
// Contrast pivots around this mid-grey, a standard 18% linear reference.
const CONTRAST_PIVOT: f32 = 0.18;

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, LUMA_WEIGHTS);
}

// Cheap approximate white balance: `temperature` > 0 warms (more red, less
// blue), `tint` > 0 pushes green. Both neutral at 0.0. Not a physically
// calibrated CCT model — a small, predictable artistic knob.
fn white_balance(c: vec3<f32>, temperature: f32, tint: f32) -> vec3<f32> {
    let t = temperature * 0.1;
    let g = tint * 0.1;
    return c * vec3<f32>(1.0 + t, 1.0 + g, 1.0 - t);
}

fn apply_contrast(c: vec3<f32>, amount: f32) -> vec3<f32> {
    return max(vec3<f32>(0.0), (c - CONTRAST_PIVOT) * amount + CONTRAST_PIVOT);
}

fn apply_saturation(c: vec3<f32>, amount: f32) -> vec3<f32> {
    let l = luma(c);
    return max(vec3<f32>(0.0), mix(vec3<f32>(l), c, amount));
}

// ACES filmic tonemap fit (Narkowicz 2015) — identical to the curve the
// old tonemap.wgsl used, so an identity grade with no bloom reproduces the
// previous output exactly.
fn aces_tonemap(x: vec3<f32>) -> vec3<f32> {
    let a = 2.51;
    let b = 0.03;
    let c = 2.43;
    let d = 0.59;
    let e = 0.14;
    return clamp((x * (a * x + b)) / (x * (c * x + d) + e), vec3<f32>(0.0), vec3<f32>(1.0));
}

// Sobel gradient magnitude of scene luma around `uv`. `step` is the sample
// spacing in UV units (texel size scaled by outline thickness). Catches
// high-contrast edges in the lit image — silhouettes plus some texture
// detail; a depth/normal-buffer outline is future work.
fn sobel_luma(uv: vec2<f32>, step: vec2<f32>) -> f32 {
    let tl = luma(textureSample(hdr_texture, linear_sampler, uv + vec2<f32>(-step.x, -step.y)).rgb);
    let tm = luma(textureSample(hdr_texture, linear_sampler, uv + vec2<f32>(0.0, -step.y)).rgb);
    let tr = luma(textureSample(hdr_texture, linear_sampler, uv + vec2<f32>(step.x, -step.y)).rgb);
    let ml = luma(textureSample(hdr_texture, linear_sampler, uv + vec2<f32>(-step.x, 0.0)).rgb);
    let mr = luma(textureSample(hdr_texture, linear_sampler, uv + vec2<f32>(step.x, 0.0)).rgb);
    let bl = luma(textureSample(hdr_texture, linear_sampler, uv + vec2<f32>(-step.x, step.y)).rgb);
    let bm = luma(textureSample(hdr_texture, linear_sampler, uv + vec2<f32>(0.0, step.y)).rgb);
    let br = luma(textureSample(hdr_texture, linear_sampler, uv + vec2<f32>(step.x, step.y)).rgb);

    let gx = (tr + 2.0 * mr + br) - (tl + 2.0 * ml + bl);
    let gy = (bl + 2.0 * bm + br) - (tl + 2.0 * tm + tr);
    return sqrt(gx * gx + gy * gy);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let scene = textureSample(hdr_texture, linear_sampler, in.uv).rgb;
    let bloom = textureSample(bloom_texture, linear_sampler, in.uv).rgb;

    var color = scene + bloom * post.bloom_intensity;
    color = color * post.exposure;
    color = white_balance(color, post.grade_temperature, post.grade_tint);
    color = color * post.grade_color_filter;
    color = apply_contrast(color, post.grade_contrast);
    color = apply_saturation(color, post.grade_saturation);

    var ldr = aces_tonemap(color);

    // Outline runs on the raw scene (pre-grade) so grading can't shift
    // which pixels read as edges; mixed into the tonemapped result as a
    // flat ink colour.
    let edge = sobel_luma(in.uv, post.texel_size * post.outline_thickness);
    let edge_factor = smoothstep(post.outline_threshold, post.outline_threshold + 0.1, edge)
        * post.outline_intensity;
    ldr = mix(ldr, post.outline_color, clamp(edge_factor, 0.0, 1.0));

    // No manual gamma encode: the swapchain's sRGB-format view applies it
    // on write, the same convention the old tonemap pass relied on.
    return vec4<f32>(ldr, 1.0);
}
