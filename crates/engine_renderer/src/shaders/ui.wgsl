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
    // Corner radius and outline width in pixels. Both zero: a plain quad.
    @location(3) shape: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) tint: vec4<f32>,
    // Pixels from the quad's centre.
    @location(2) local: vec2<f32>,
    @location(3) @interpolate(flat) half_size: vec2<f32>,
    @location(4) @interpolate(flat) shape: vec2<f32>,
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
    out.half_size = instance.rect.zw * 0.5;
    out.local = (corner - vec2<f32>(0.5, 0.5)) * instance.rect.zw;
    out.shape = instance.shape.xy;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let sampled = textureSample(atlas_texture, atlas_sampler, in.uv);
    var color = sampled * in.tint;
    if in.shape.x > 0.0 || in.shape.y > 0.0 {
        // Distance to a rounded box's edge, negative inside; a one-pixel
        // ramp across the edge is the anti-aliasing.
        let radius = min(in.shape.x, min(in.half_size.x, in.half_size.y));
        let q = abs(in.local) - in.half_size + vec2<f32>(radius, radius);
        let distance = length(max(q, vec2<f32>(0.0, 0.0))) + min(max(q.x, q.y), 0.0) - radius;
        var coverage = clamp(0.5 - distance, 0.0, 1.0);
        if in.shape.y > 0.0 {
            coverage = coverage * clamp(0.5 + distance + in.shape.y, 0.0, 1.0);
        }
        color.a = color.a * coverage;
    }
    // Fully transparent fragments are discarded rather than blended, so
    // the gaps between glyph strokes cost nothing.
    if color.a <= 0.001 {
        discard;
    }
    return color;
}
