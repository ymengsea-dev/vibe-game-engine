// Shared PBR bindings, lighting math, and fragment stage — everything the
// unskinned (`pbr_vs.wgsl`) and GPU-skinned (`skinned_pbr_vs.wgsl`)
// vertex stages have in common. The renderer concatenates one vertex-stage
// file onto this one at pipeline creation; this file is never compiled
// alone (it declares no `vs_main`).
//
// Fragment stage: samples a base color texture and shades with a
// physically-based (Cook-Torrance GGX) BRDF driven by the material's
// metallic-roughness factors and the scene's directional/point lights.

struct CameraUniform {
    view_proj: mat4x4<f32>,
    // xyz = world-space eye position; w unused.
    view_position: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> camera: CameraUniform;

@group(1) @binding(0)
var base_color_texture: texture_2d<f32>;
@group(1) @binding(1)
var base_color_sampler: sampler;

struct MaterialUniform {
    base_color_factor: vec4<f32>,
    metallic_factor: f32,
    roughness_factor: f32,
    // Alpha below which a masked fragment is discarded.
    alpha_cutoff: f32,
    // 0 opaque, 1 mask, 2 blend. Mirrors `AlphaMode` in `material.rs`.
    alpha_mode: u32,
    emissive_factor: vec3<f32>,
    normal_scale: f32,
    occlusion_strength: f32,
    // No trailing padding field here on purpose. WGSL aligns `vec3` to
    // 16 bytes, so a `_padding: vec3<f32>` would start a fifth row and
    // make the struct 80 bytes against Rust's 64. Omitting it lets WGSL
    // round the struct up to 64, matching `MaterialUniform` exactly —
    // whose own `_padding: [f32; 3]` is what fills those last 12 bytes.
};

@group(1) @binding(2)
var<uniform> material: MaterialUniform;

// Optional maps. When a material has none, these are bound to neutral
// 1x1 textures (flat normal, black, white) so the maths below needs no
// branch — see `Pipeline`'s `NeutralMaps`. All three share
// `base_color_sampler`.
@group(1) @binding(3)
var normal_map: texture_2d<f32>;
@group(1) @binding(4)
var emissive_map: texture_2d<f32>;
@group(1) @binding(5)
var occlusion_map: texture_2d<f32>;

// Perturbs `n` by a tangent-space normal map sample.
//
// The tangent basis is derived from screen-space derivatives rather than
// imported per-vertex tangents: glTF tangents are optional, and this
// works for any mesh without changing the vertex format. Slightly less
// exact on heavily distorted UVs, and the cost is two extra derivative
// pairs.
fn apply_normal_map(n: vec3<f32>, world_position: vec3<f32>, uv: vec2<f32>, scale: f32) -> vec3<f32> {
    let sampled = textureSample(normal_map, base_color_sampler, uv).xyz * 2.0 - 1.0;
    // A neutral map samples to (0, 0, 1), so this is a no-op for it.
    if scale <= 0.0 || (abs(sampled.x) < 0.001 && abs(sampled.y) < 0.001) {
        return n;
    }

    let dp1 = dpdx(world_position);
    let dp2 = dpdy(world_position);
    let duv1 = dpdx(uv);
    let duv2 = dpdy(uv);

    let determinant = duv1.x * duv2.y - duv2.x * duv1.y;
    if abs(determinant) < 1e-8 {
        return n;
    }
    let inv = 1.0 / determinant;
    let tangent = normalize((dp1 * duv2.y - dp2 * duv1.y) * inv);
    let bitangent = normalize((dp2 * duv1.x - dp1 * duv2.x) * inv);

    let perturbed = normalize(
        tangent * sampled.x * scale + bitangent * sampled.y * scale + n * sampled.z
    );
    return perturbed;
}

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
const MAX_POINT_LIGHTS: u32 = 4u;

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
};

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

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let tex_color = textureSample(base_color_texture, base_color_sampler, in.uv);
    let base_color = tex_color * material.base_color_factor;

    // Cutout: kill the fragment before doing any lighting work for it.
    // Mode 1 is `AlphaMode::Mask`.
    if material.alpha_mode == 1u && base_color.a < material.alpha_cutoff {
        discard;
    }

    let albedo = base_color.rgb;
    let metallic = clamp(material.metallic_factor, 0.0, 1.0);
    let roughness = clamp(material.roughness_factor, 0.045, 1.0);

    let geometric_normal = normalize(in.world_normal);
    let n = apply_normal_map(
        geometric_normal,
        in.world_position,
        in.uv,
        material.normal_scale,
    );
    let v = normalize(camera.view_position.xyz - in.world_position);
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
            contribution *= shadow_factor(in.world_position);
        }
        direct_light += contribution;
    }

    for (var i = 0u; i < lights.point_count; i++) {
        let light = lights.point_lights[i];
        let to_light = light.position - in.world_position;
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
    let occlusion_sample = textureSample(occlusion_map, base_color_sampler, in.uv).r;
    let occlusion = mix(1.0, occlusion_sample, clamp(material.occlusion_strength, 0.0, 1.0));
    let ambient = albedo * ambient_color * lights.ambient_intensity * occlusion;

    // Emission is added last: it is light leaving the surface, so it is
    // affected by neither shadowing nor occlusion.
    let emissive = textureSample(emissive_map, base_color_sampler, in.uv).rgb
        * material.emissive_factor;

    // A masked fragment that survived the cutoff is fully opaque; only
    // blend mode carries alpha through.
    var out_alpha = 1.0;
    if material.alpha_mode == 2u {
        out_alpha = base_color.a;
    }
    return vec4<f32>(ambient + direct_light + emissive, out_alpha);
}
