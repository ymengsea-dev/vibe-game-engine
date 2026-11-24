// Shared PBR bindings and lighting math: camera, lights, shadow map, the
// vertex-to-fragment interface and the BRDF, ending in `shade_surface`.
// Every lit pipeline builds on it: `pbr_material_fs.wgsl` adds the
// material fragment stage (together they are `PBR_COMMON_SOURCE`), and
// `terrain_fs.wgsl` adds the terrain's paint-layer fragment stage. A
// vertex-stage file is concatenated on at pipeline creation; this file is
// never compiled alone (it declares no entry points).

struct CameraUniform {
    view_proj: mat4x4<f32>,
    // xyz = world-space eye position; w unused.
    view_position: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> camera: CameraUniform;

// `@group(2)` is owned by the vertex stage: the unskinned and skinned
// paths put a per-object model matrix there (`pbr_vs.wgsl` /
// `skinned_pbr_vs.wgsl`), the instanced path (`instanced_pbr_vs.wgsl`)
// uses none. Declaring it here would force every pipeline layout to bind
// it, so each vertex file declares only what it uses.

struct DirectionalLight {
    direction: vec3<f32>,
    color: vec3<f32>,
    intensity: f32,
};

struct PointLight {
    position: vec3<f32>,
    range: f32,
    color: vec3<f32>,
    intensity: f32,
};

const MAX_DIRECTIONAL_LIGHTS: u32 = 2u;
const MAX_POINT_LIGHTS: u32 = 16u;

struct LightsUniform {
    directional_count: u32,
    point_count: u32,
    directional_lights: array<DirectionalLight, MAX_DIRECTIONAL_LIGHTS>,
    point_lights: array<PointLight, MAX_POINT_LIGHTS>,
    // Hemisphere ambient. Appended after the arrays so every field above
    // keeps the offset it had before ambient existed.
    ambient_sky: vec3<f32>,
    ambient_intensity: f32,
    ambient_ground: vec3<f32>,
    _padding1: f32,
};

@group(3) @binding(0)
var<uniform> lights: LightsUniform;

// Shadow map for the scene's first directional light only (index 0) —
// other lights are unshadowed. Multiple shadow-casting lights would need
// one map (and one sampling pass) each; future work.
@group(3) @binding(1)
var shadow_map: texture_depth_2d;
@group(3) @binding(2)
var shadow_sampler: sampler_comparison;

struct ShadowUniform {
    light_space_matrix: mat4x4<f32>,
};

@group(3) @binding(3)
var<uniform> shadow: ShadowUniform;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_normal: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) world_position: vec3<f32>,
    // Distance fade: 1 = fully drawn, 0 = gone. Constant per object.
    @location(3) @interpolate(flat) fade: f32,
};

// 4x4 ordered-dither threshold in (0, 1) for the pixel at `position`.
// Shared by the scene and shadow passes so a fading object's shadow
// thins out in step with it.
fn dither_threshold(position: vec2<f32>) -> f32 {
    let bayer = array<f32, 16>(
        0.0, 8.0, 2.0, 10.0,
        12.0, 4.0, 14.0, 6.0,
        3.0, 11.0, 1.0, 9.0,
        15.0, 7.0, 13.0, 5.0,
    );
    let cell = vec2<u32>(position) % vec2<u32>(4u);
    return (bayer[cell.y * 4u + cell.x] + 0.5) / 16.0;
}

const PI: f32 = 3.14159265359;

// Trowbridge-Reitz GGX normal distribution: how many microfacets are
// aligned with the halfway vector `h`. Higher for smooth (low-roughness)
// surfaces, concentrating reflection into a tight highlight.
fn distribution_ggx(n_dot_h: f32, roughness: f32) -> f32 {
    let a = roughness * roughness;
    let a2 = a * a;
    let denom = n_dot_h * n_dot_h * (a2 - 1.0) + 1.0;
    return a2 / max(PI * denom * denom, 0.0000001);
}

// Schlick-GGX geometry term: self-shadowing/masking of microfacets.
fn geometry_schlick_ggx(n_dot_x: f32, roughness: f32) -> f32 {
    let r = roughness + 1.0;
    let k = (r * r) / 8.0;
    return n_dot_x / max(n_dot_x * (1.0 - k) + k, 0.0000001);
}

// Smith's method: combines view-direction and light-direction masking.
fn geometry_smith(n_dot_v: f32, n_dot_l: f32, roughness: f32) -> f32 {
    return geometry_schlick_ggx(n_dot_v, roughness) * geometry_schlick_ggx(n_dot_l, roughness);
}

// Fresnel-Schlick: how much light reflects (vs. refracts) at this angle.
// Grazing angles reflect more, regardless of material.
fn fresnel_schlick(cos_theta: f32, f0: vec3<f32>) -> vec3<f32> {
    return f0 + (vec3<f32>(1.0) - f0) * pow(clamp(1.0 - cos_theta, 0.0, 1.0), 5.0);
}

// One light's contribution to outgoing radiance at a point, given its
// incoming direction `l` (pointing *toward* the light) and pre-attenuated
// `radiance` (color * intensity, with distance falloff already applied
// for point lights).
fn shade_light(
    l: vec3<f32>,
    radiance: vec3<f32>,
    n: vec3<f32>,
    v: vec3<f32>,
    albedo: vec3<f32>,
    metallic: f32,
    roughness: f32,
    f0: vec3<f32>,
) -> vec3<f32> {
    let h = normalize(v + l);
    let n_dot_l = max(dot(n, l), 0.0);
    let n_dot_v = max(dot(n, v), 0.0);
    let n_dot_h = max(dot(n, h), 0.0);

    if n_dot_l <= 0.0 {
        return vec3<f32>(0.0);
    }

    let d = distribution_ggx(n_dot_h, roughness);
    let g = geometry_smith(n_dot_v, n_dot_l, roughness);
    let f = fresnel_schlick(max(dot(h, v), 0.0), f0);

    let specular = (d * g * f) / max(4.0 * n_dot_v * n_dot_l, 0.0000001);

    // Energy conservation: reflected light (specular, via `f`) isn't also
    // refracted (diffuse). Metals have no diffuse term at all.
    let k_diffuse = (vec3<f32>(1.0) - f) * (1.0 - metallic);
    let diffuse = k_diffuse * albedo / PI;

    return (diffuse + specular) * radiance * n_dot_l;
}

// How lit `world_position` is by the shadow-casting light, in [0, 1]
// (`0` = fully shadowed, `1` = fully lit). Points outside the light's
// orthographic frustum are treated as unshadowed rather than clamped to
// the map's edge texel, which would otherwise smear the nearest edge
// shadow across everything past the frustum bounds.
fn shadow_factor(world_position: vec3<f32>) -> f32 {
    let light_clip = shadow.light_space_matrix * vec4<f32>(world_position, 1.0);
    let light_ndc = light_clip.xyz / light_clip.w;

    if light_ndc.x < -1.0 || light_ndc.x > 1.0 || light_ndc.y < -1.0 || light_ndc.y > 1.0
        || light_ndc.z < 0.0 || light_ndc.z > 1.0 {
        return 1.0;
    }

    // wgpu texture space is [0,1] with v=0 at the top; NDC y=+1 is also
    // "up", so v needs the sign flip x doesn't.
    let shadow_uv = vec2<f32>(light_ndc.x * 0.5 + 0.5, light_ndc.y * -0.5 + 0.5);
    let current_depth = light_ndc.z;

    // 3x3 PCF: average 9 taps around the sample point to soften the hard
    // shadow-map edge into a smoother penumbra-like falloff.
    let texel = 1.0 / vec2<f32>(textureDimensions(shadow_map));
    var lit = 0.0;
    for (var dx = -1; dx <= 1; dx++) {
        for (var dy = -1; dy <= 1; dy++) {
            let offset = vec2<f32>(f32(dx), f32(dy)) * texel;
            lit += textureSampleCompare(shadow_map, shadow_sampler, shadow_uv + offset, current_depth);
        }
    }
    return lit / 9.0;
}

// Direct light from every scene light (the first directional light
// shadowed) plus hemisphere ambient scaled by `occlusion`, for a surface
// with this `albedo`, `metallic`/`roughness` and unit normal `n`.
fn shade_surface(
    albedo: vec3<f32>,
    metallic: f32,
    roughness: f32,
    n: vec3<f32>,
    world_position: vec3<f32>,
    occlusion: f32,
) -> vec3<f32> {
    let v = normalize(camera.view_position.xyz - world_position);
    // Dielectrics (non-metals) reflect ~4% at normal incidence regardless
    // of color; metals tint their reflection with their own albedo.
    let f0 = mix(vec3<f32>(0.04), albedo, metallic);

    var direct_light = vec3<f32>(0.0);

    for (var i = 0u; i < lights.directional_count; i++) {
        let light = lights.directional_lights[i];
        // `direction` is where the light travels *toward*; the BRDF
        // wants the direction *toward the light source*.
        let l = normalize(-light.direction);
        let radiance = light.color * light.intensity;
        var contribution = shade_light(l, radiance, n, v, albedo, metallic, roughness, f0);
        if i == 0u {
            contribution *= shadow_factor(world_position);
        }
        direct_light += contribution;
    }

    for (var i = 0u; i < lights.point_count; i++) {
        let light = lights.point_lights[i];
        let to_light = light.position - world_position;
        let distance = max(length(to_light), 0.0001);
        let l = to_light / distance;
        let attenuation = 1.0 / (distance * distance);
        let radiance = light.color * light.intensity * attenuation;
        direct_light += shade_light(l, radiance, n, v, albedo, metallic, roughness, f0);
    }

    // Hemisphere ambient: sky colour from above, ground bounce from
    // below, mixed by how much this surface faces up. Not real GI, but
    // enough to separate "in shade outdoors" from "in a cave" — the flat
    // `albedo * 0.03` this replaced made every unlit face near-black.
    //
    // Mirrored on the CPU by `AmbientLight::color_for_normal` in
    // `light.rs`, which is what the tests exercise. Keep the two in step.
    let up_factor = clamp(n.y * 0.5 + 0.5, 0.0, 1.0);
    let ambient_color = mix(lights.ambient_ground, lights.ambient_sky, up_factor);

    // Ambient occlusion darkens *indirect* light only — direct light has
    // its own shadowing and would double-darken.
    let ambient = albedo * ambient_color * lights.ambient_intensity * occlusion;
    return ambient + direct_light;
}
