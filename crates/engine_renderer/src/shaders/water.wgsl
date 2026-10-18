struct Camera { view_projection: mat4x4<f32>, view_position: vec4<f32> };
struct Model { model: mat4x4<f32> };
struct Water { center_height: vec4<f32>, extent_wave: vec4<f32>, direction_time: vec4<f32>, shallow: vec4<f32>, deep: vec4<f32> };
struct Impulse { position_radius: vec4<f32>, strength_age: vec4<f32> };
struct Impulses { entries: array<Impulse, 64> };
@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var<uniform> water: Water;
@group(1) @binding(1) var<uniform> impulses: Impulses;
@group(2) @binding(0) var<uniform> model: Model;
struct In { @location(0) position: vec3<f32>, @location(1) normal: vec3<f32> };
struct Out { @builtin(position) position: vec4<f32>, @location(0) world: vec3<f32>, @location(1) normal: vec3<f32>, @location(2) edge_depth: f32 };

fn ripple(world_xz: vec2<f32>) -> f32 {
  var result = 0.0;
  for (var i = 0u; i < 64u; i = i + 1u) {
    let impulse = impulses.entries[i];
    let radius = impulse.position_radius.w;
    let strength = impulse.strength_age.x;
    let age = impulse.strength_age.y;
    if (radius > 0.0 && strength > 0.0 && age >= 0.0) {
      let distance = distance(world_xz, impulse.position_radius.xz);
      let falloff = max(1.0 - distance / radius, 0.0);
      result += sin(distance * 7.0 - age * 8.0) * falloff * falloff * strength * exp(-age * 1.8);
    }
  }
  return result;
}

@vertex fn vs_main(input: In) -> Out {
  let base_world = model.model * vec4<f32>(input.position, 1.0);
  let local_xz = base_world.xz - water.center_height.xz;
  let k = 6.2831853 / max(water.extent_wave.z, 0.0001);
  let phase = dot(local_xz, water.direction_time.xy) * k + water.direction_time.z * water.direction_time.w;
  let wave_height = sin(phase) * water.extent_wave.y;
  let slope = water.direction_time.xy * (cos(phase) * water.extent_wave.y * k);
  let world = base_world + vec4<f32>(0.0, wave_height + ripple(base_world.xz), 0.0, 0.0);
  var out: Out;
  out.position = camera.view_projection * world;
  out.world = world.xyz;
  out.normal = normalize(vec3<f32>(-slope.x, 1.0, -slope.y));
  out.edge_depth = 1.0 - max(abs(input.position.x), abs(input.position.z));
  return out;
}
@fragment fn fs_main(input: Out) -> @location(0) vec4<f32> {
  let depth = clamp(input.edge_depth, 0.0, 1.0);
  let base_color = mix(water.shallow.rgb, water.deep.rgb, depth);
  let view = normalize(camera.view_position.xyz - input.world);
  let fresnel = pow(1.0 - max(dot(input.normal, view), 0.0), 4.0);
  let foam = smoothstep(0.92, 1.0, 1.0 - input.edge_depth);
  return vec4<f32>(base_color + fresnel * 0.2 + foam * 0.12, 1.0);
}
