// Screen-space UI: coloured quads, textured quads, and text glyphs.
//
// One pipeline for all three. Every instance carries a rect in pixels, a
// UV sub-rect into an atlas, a tint, and a mode. Solid fills point at a
// white texel, so nothing needs a second pipeline or a branch in the
// vertex stage.

struct ScreenUniform {
    // Reciprocal screen size, for pixels -> clip space.
    inv_size: vec2<f32>,
    _padding: vec2<f32>,
};

@group(0) @binding(0)
var<uniform> screen: ScreenUniform;

@group(1) @binding(0)
var atlas_texture: texture_2d<f32>;
@group(1) @binding(1)
var atlas_sampler: sampler;

struct Instance {
    // Rect in pixels: x, y, width, height. Origin is the top-left of the
    // window, y growing downward, matching how UI layout is authored.
    @location(0) rect: vec4<f32>,
    // Atlas sub-rect: min u, min v, max u, max v.
    @location(1) uv: vec4<f32>,
    @location(2) tint: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) tint: vec4<f32>,
};

@vertex
fn vs_main(
    @builtin(vertex_index) vertex_index: u32,
    instance: Instance,
) -> VertexOutput {
    // Two triangles as a strip-like index pattern, expanded from the
    // vertex index so no vertex buffer is needed.
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 1.0),
    );
    let corner = corners[vertex_index];

    let pixel = instance.rect.xy + corner * instance.rect.zw;
    // Pixels -> normalized -> clip space, flipping y because UI counts
    // downward while clip space counts up.
    let normalized = pixel * screen.inv_size;
    let clip = vec2<f32>(normalized.x * 2.0 - 1.0, 1.0 - normalized.y * 2.0);

    var out: VertexOutput;
    out.clip_position = vec4<f32>(clip, 0.0, 1.0);
    out.uv = mix(instance.uv.xy, instance.uv.zw, corner);
    out.tint = instance.tint;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let sampled = textureSample(atlas_texture, atlas_sampler, in.uv);
    let color = sampled * in.tint;
    // Fully transparent fragments are discarded rather than blended, so
    // the gaps between glyph strokes cost nothing.
    if color.a <= 0.001 {
        discard;
    }
    return color;
}
