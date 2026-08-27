// Physics debug-line pipeline: draws pre-colored line segments (collider
// wireframes, rigid body axes, joints — see `engine_physics::DebugLine`)
// with no lighting, always on top of the rest of the scene (drawn last,
// same as `pbr.wgsl`'s geometry — no depth buffer to test against yet).

// Reuses `pbr.wgsl`'s camera bind group (same buffer, same binding slot)
// — this shader only reads the leading `view_proj` field, so it declares
// a smaller local struct than `pbr.wgsl`'s full `CameraUniform` rather
// than duplicating the `view_position` field it never uses.
struct CameraUniform {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0)
var<uniform> camera: CameraUniform;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) color: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.clip_position = camera.view_proj * vec4<f32>(in.position, 1.0);
    out.color = in.color;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return in.color;
}
