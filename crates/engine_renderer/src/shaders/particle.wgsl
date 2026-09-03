// Billboard particle pipeline: each instance is one point (position, size,
// colour, roll) expanded in the vertex stage into a camera-facing quad
// using the world-space right/up axes handed in via the uniform. No vertex
// buffer for the quad itself — six positions come from `vertex_index`.
//
// The fragment stage shapes every quad into a soft round blob with a
// smoothstep falloff from the centre (no texture), and outputs
// premultiplied colour so one shader serves both the alpha-blended and the
// additive pipeline — only their `BlendState` differs (see `particle.rs`).
//
// Drawn inside the scene pass, into the HDR target, so the post-processing
// bloom and tonemap treat bright particles like any other radiance.

struct ParticleCamera {
    view_projection: mat4x4<f32>,
    camera_right: vec4<f32>,
    camera_up: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> camera: ParticleCamera;

struct InstanceInput {
    @location(0) position: vec3<f32>,
    @location(1) size: f32,
    @location(2) color: vec4<f32>,
    @location(3) rotation: f32,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) quad_uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32, instance: InstanceInput) -> VertexOutput {
    // Two triangles covering [-1, 1] on each axis of the billboard plane.
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(1.0, -1.0),
        vec2<f32>(-1.0, 1.0),
        vec2<f32>(-1.0, 1.0),
        vec2<f32>(1.0, -1.0),
        vec2<f32>(1.0, 1.0),
    );
    let corner = corners[index];

    // Roll the corner within the billboard plane.
    let s = sin(instance.rotation);
    let c = cos(instance.rotation);
    let rolled = vec2<f32>(corner.x * c - corner.y * s, corner.x * s + corner.y * c);

    let half = instance.size * 0.5;
    let world = instance.position
        + camera.camera_right.xyz * (rolled.x * half)
        + camera.camera_up.xyz * (rolled.y * half);

    var out: VertexOutput;
    out.clip_position = camera.view_projection * vec4<f32>(world, 1.0);
    // Unrolled corner -> [0, 1] quad UV, so the soft-circle mask stays
    // centred regardless of roll.
    out.quad_uv = corner * 0.5 + vec2<f32>(0.5, 0.5);
    out.color = instance.color;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Distance from quad centre, ~0 at centre to ~1 at the edge midpoints.
    let dist = distance(in.quad_uv, vec2<f32>(0.5, 0.5)) * 2.0;
    // Soft round falloff: opaque core, feathered rim, nothing past the edge.
    let mask = smoothstep(1.0, 0.35, dist);
    let alpha = clamp(in.color.a * mask, 0.0, 1.0);
    // Premultiplied output: the alpha pipeline blends (One, OneMinusSrcAlpha)
    // and the additive one (One, One) — both correct for this.
    return vec4<f32>(in.color.rgb * alpha, alpha);
}
