// Procedural skybox: a full-screen triangle (no vertex/index buffer)
// painted with a horizon-to-zenith gradient plus a glow toward the sun,
// keyed entirely off a per-pixel view ray reconstructed from the inverse
// view-projection matrix. Drawn first in the main color pass, before any
// scene geometry, so opaque objects paint over it.

struct SkyboxUniform {
    inverse_view_proj: mat4x4<f32>,
    camera_position: vec4<f32>,
    // xyz = direction the sun travels *toward* (not toward the source).
    sun_direction: vec4<f32>,
    sun_color: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> sky: SkyboxUniform;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) ndc: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOutput {
    // Oversized triangle covering the whole viewport in one draw — the
    // standard "full-screen triangle" trick, cheaper than a quad (no
    // diagonal seam, no index buffer).
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    let pos = positions[index];

    var out: VertexOutput;
    // z=1, w=1: the far plane in wgpu's [0,1] depth range — correct for
    // "infinitely far background" even though this pipeline has no depth
    // attachment to test it against.
    out.clip_position = vec4<f32>(pos, 1.0, 1.0);
    out.ndc = pos;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Unproject this pixel's far-plane clip-space point back to world
    // space, then subtract the camera position to get the view ray.
    let far_point_clip = vec4<f32>(in.ndc, 1.0, 1.0);
    let far_point_world = sky.inverse_view_proj * far_point_clip;
    let world_point = far_point_world.xyz / far_point_world.w;
    let ray = normalize(world_point - sky.camera_position.xyz);

    let horizon_color = vec3<f32>(0.65, 0.75, 0.85);
    let zenith_color = vec3<f32>(0.10, 0.25, 0.55);
    let height = clamp(ray.y * 0.5 + 0.5, 0.0, 1.0);
    var color = mix(horizon_color, zenith_color, height);

    // Sun glow: brightest looking toward the light source (opposite its
    // travel direction), falling off sharply with angle.
    let to_sun = normalize(-sky.sun_direction.xyz);
    let sun_amount = pow(max(dot(ray, to_sun), 0.0), 256.0);
    color += sky.sun_color.rgb * sun_amount * 4.0;

    return vec4<f32>(color, 1.0);
}
