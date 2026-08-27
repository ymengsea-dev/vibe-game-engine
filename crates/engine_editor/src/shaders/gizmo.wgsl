// Translate gizmo: draws pre-colored axis-handle line segments with no
// lighting, on top of the rest of the viewport.
//
// Reuses the viewport cube pipeline's camera bind group (same buffer,
// same binding slot as `viewport.wgsl`) — this shader only reads the
// leading `view_proj` field, so it declares a smaller local struct than
// `viewport.wgsl`'s full `CameraUniform` rather than duplicating the
// `view_position` field it never uses (same trick `debug_line.wgsl`
// uses against the main game pipeline's camera).

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
    // Manual gamma encoding, same reason `viewport.wgsl` does it: this
    // pipeline's target is `Rgba8Unorm`, which (unlike the main game
    // pipeline's `Bgra8UnormSrgb` swapchain) has no hardware sRGB
    // auto-conversion.
    let encoded = pow(in.color.rgb, vec3<f32>(1.0 / 2.2));
    return vec4<f32>(encoded, in.color.a);
}
