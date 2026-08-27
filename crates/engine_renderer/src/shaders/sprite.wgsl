// Instanced textured-quad sprite shader: one static unit quad (a `Vertex`
// buffer — position/normal/uv; normal unused, sprites are unlit) is
// stamped out per instance at a world-space position/size/rotation
// (`InstanceInput`, one `SpriteInstance` per draw), sampling `uv_min`..
// `uv_max` of a shared atlas texture and tinting by `color`. Drawn with
// alpha blending (see `SpritePipeline`'s blend state) — unlike
// `pbr.wgsl`'s opaque geometry, sprites need real transparency.

struct CameraUniform {
    view_proj: mat4x4<f32>,
    // xyz = world-space eye position; w unused. Unread here (sprites
    // aren't lit), kept only so this struct's layout matches the shared
    // `CameraUniform` buffer `pbr.wgsl`'s camera binding also uses.
    view_position: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> camera: CameraUniform;

@group(1) @binding(0)
var atlas_texture: texture_2d<f32>;
@group(1) @binding(1)
var atlas_sampler: sampler;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
};

struct InstanceInput {
    @location(3) position: vec3<f32>,
    @location(4) size: vec2<f32>,
    @location(5) rotation: f32,
    @location(6) uv_min: vec2<f32>,
    @location(7) uv_max: vec2<f32>,
    @location(8) color: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_main(vertex: VertexInput, instance: InstanceInput) -> VertexOutput {
    let c = cos(instance.rotation);
    let s = sin(instance.rotation);
    let local = vertex.position.xy * instance.size;
    let rotated = vec2<f32>(local.x * c - local.y * s, local.x * s + local.y * c);
    let world_position = instance.position + vec3<f32>(rotated, 0.0);

    var out: VertexOutput;
    out.clip_position = camera.view_proj * vec4<f32>(world_position, 1.0);
    out.uv = mix(instance.uv_min, instance.uv_max, vertex.uv);
    out.color = instance.color;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let sampled = textureSample(atlas_texture, atlas_sampler, in.uv);
    return sampled * in.color;
}
