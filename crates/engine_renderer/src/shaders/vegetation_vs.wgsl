// Wind-animated instanced vertex stage for the PBR shader. Concatenated
// onto `pbr_common.wgsl` (which declares `camera` and `VertexOutput`) by
// `GpuContext::create_vegetation_pipeline`.
//
// Like `instanced_pbr_vs.wgsl`, the per-plant model matrix comes from an
// instance-step vertex buffer (`InstanceRaw`, locations 3..6). On top of
// that this stage bends each vertex along the wind direction: the plant's
// base (object-space y = 0) stays put and the sway grows with height, so a
// blade pivots at its root. A per-plant phase derived from the instance's
// world origin keeps a field of blades from swaying in lockstep.

struct WindUniform {
    time: f32,
    strength: f32,
    frequency: f32,
    turbulence: f32,
    // Normalized wind heading on the ground plane (x, z).
    direction: vec2<f32>,
    _padding: vec2<f32>,
};

@group(2) @binding(0)
var<uniform> wind: WindUniform;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) model_col0: vec4<f32>,
    @location(4) model_col1: vec4<f32>,
    @location(5) model_col2: vec4<f32>,
    @location(6) model_col3: vec4<f32>,
};

// Sway offset (in world units, along the wind direction) for a vertex at
// object-space height `local_y` on a plant whose world origin gives phase
// `phase`. Mirrored on the CPU by `vegetation::wind_sway` for testing.
fn wind_sway(local_y: f32, phase: f32) -> f32 {
    let bend = max(local_y, 0.0);
    let primary = sin(wind.time * wind.frequency + phase);
    let flutter = sin(wind.time * wind.frequency * 3.7 + phase * 2.3) * wind.turbulence;
    return (primary + flutter) * wind.strength * bend;
}

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;

    let model = mat4x4<f32>(in.model_col0, in.model_col1, in.model_col2, in.model_col3);
    let plant_origin = model[3].xyz;
    let phase = plant_origin.x * 0.7 + plant_origin.z * 0.7;

    let sway = wind_sway(in.position.y, phase);
    let wind_dir = vec3<f32>(wind.direction.x, 0.0, wind.direction.y);

    var local = in.position + wind_dir * sway;
    // Cheap arc correction: as the blade leans, pull its tip down a little
    // so it doesn't visibly stretch.
    local.y = local.y - abs(sway) * max(in.position.y, 0.0) * 0.15;

    let world_position = model * vec4<f32>(local, 1.0);
    out.clip_position = camera.view_proj * world_position;
    out.world_position = world_position.xyz;
    // No normal-matrix / bend-normal correction — imperceptible for thin
    // blades (see the vegetation module docs).
    out.world_normal = (model * vec4<f32>(in.normal, 0.0)).xyz;
    out.uv = in.uv;
    return out;
}
