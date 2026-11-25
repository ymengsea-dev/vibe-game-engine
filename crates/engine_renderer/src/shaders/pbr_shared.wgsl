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
    // xyz = fog colour, w = distance fog density.
    fog_color_density: vec4<f32>,
    // fog start distance, height density, height, height falloff.
    fog_height: vec4<f32>,
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
    // Shadow slot (see `MAX_SHADOWED_LIGHTS`), or -1 for none.
    shadow_slot: i32,
};

struct SpotLight {
    position: vec3<f32>,
    range: f32,
    direction: vec3<f32>,
    cos_outer: f32,
    color: vec3<f32>,
    intensity: f32,
    cos_inner: f32,
    shadow_slot: i32,
};

const MAX_DIRECTIONAL_LIGHTS: u32 = 2u;
const MAX_POINT_LIGHTS: u32 = 16u;
const MAX_SPOT_LIGHTS: u32 = 8u;
const MAX_SHADOWED_LIGHTS: u32 = 4u;
const MAX_CASCADES: u32 = 4u;
// Shadow-map layers per shadowed light: six faces for a point light.
const LAYERS_PER_LIGHT: u32 = 6u;

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
    spot_count: u32,
    spot_lights: array<SpotLight, MAX_SPOT_LIGHTS>,
};

@group(3) @binding(0)
var<uniform> lights: LightsUniform;

// The sun's (first directional light's) cascade maps, one array layer per
// cascade, nearest first. Other directional lights are unshadowed.
@group(3) @binding(1)
var shadow_map: texture_depth_2d_array;
@group(3) @binding(2)
var shadow_sampler: sampler_comparison;

struct ShadowsUniform {
    cascade_matrices: array<mat4x4<f32>, MAX_CASCADES>,
    // Point and spot light maps, layer `slot * LAYERS_PER_LIGHT + face`.
    local_matrices: array<mat4x4<f32>, 24>,
    // Where each cascade ends, measured from the camera.
    cascade_splits: vec4<f32>,
    cascade_count: u32,
};

@group(3) @binding(3)
var<uniform> shadow: ShadowsUniform;

// Point and spot light maps.
@group(3) @binding(4)
var local_shadow_map: texture_depth_2d_array;

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

// Where `world_position` lands in a shadow map whose matrix is `matrix`:
// xy = texture uv, z = depth. Outside the map when any part leaves [0, 1].
fn shadow_coords(matrix: mat4x4<f32>, world_position: vec3<f32>) -> vec3<f32> {
    let clip = matrix * vec4<f32>(world_position, 1.0);
    let ndc = clip.xyz / clip.w;
    // wgpu texture space is [0,1] with v=0 at the top; NDC y=+1 is also
    // "up", so v needs the sign flip x doesn't.
    return vec3<f32>(ndc.x * 0.5 + 0.5, ndc.y * -0.5 + 0.5, ndc.z);
}

fn inside_map(coords: vec3<f32>) -> bool {
    return all(coords >= vec3<f32>(0.0)) && all(coords <= vec3<f32>(1.0));
}

// 3x3 PCF: average 9 taps around `coords` to soften the hard shadow-map
// edge into a smoother penumbra-like falloff. `textureSampleCompareLevel`
// rather than `textureSampleCompare`: which layer is sampled varies per
// pixel, which the derivative-taking form doesn't allow.
fn pcf(map: texture_depth_2d_array, layer: u32, coords: vec3<f32>) -> f32 {
    let texel = 1.0 / vec2<f32>(textureDimensions(map));
    var lit = 0.0;
    for (var dx = -1; dx <= 1; dx++) {
        for (var dy = -1; dy <= 1; dy++) {
            let offset = vec2<f32>(f32(dx), f32(dy)) * texel;
            lit += textureSampleCompareLevel(map, shadow_sampler, coords.xy + offset, layer, coords.z);
        }
    }
    return lit / 9.0;
}

// How lit `world_position` is by the sun, in [0, 1] (`0` = fully
// shadowed). Uses the nearest cascade whose map covers the point; past the
// last one there is no shadow, faded in over the last tenth of the shadow
// distance so the edge doesn't show as a line.
fn sun_shadow(world_position: vec3<f32>) -> f32 {
    let count = min(shadow.cascade_count, MAX_CASCADES);
    if count == 0u {
        return 1.0;
    }
    let distance = length(world_position - camera.view_position.xyz);
    let reach = shadow.cascade_splits[count - 1u];
    if distance > reach {
        return 1.0;
    }
    for (var cascade = 0u; cascade < count; cascade++) {
        let coords = shadow_coords(shadow.cascade_matrices[cascade], world_position);
        if inside_map(coords) {
            let lit = pcf(shadow_map, cascade, coords);
            let fade = smoothstep(reach * 0.9, reach, distance);
            return mix(lit, 1.0, fade);
        }
    }
    return 1.0;
}

// How lit `world_position` is by the point or spot light in shadow `slot`
// whose map layer is `layer`. Unshadowed outside that map.
fn local_shadow(layer: u32, world_position: vec3<f32>) -> f32 {
    let coords = shadow_coords(shadow.local_matrices[layer], world_position);
    if !inside_map(coords) {
        return 1.0;
    }
    return pcf(local_shadow_map, layer, coords);
}

// The point light's face for the direction `away` from it: the largest
// axis, positive before negative, as `POINT_SHADOW_FACES` in `shadow.rs`.
fn point_face(away: vec3<f32>) -> u32 {
    let a = abs(away);
    if a.x >= a.y && a.x >= a.z {
        return select(1u, 0u, away.x >= 0.0);
    }
    if a.y >= a.z {
        return select(3u, 2u, away.y >= 0.0);
    }
    return select(5u, 4u, away.z >= 0.0);
}

// A spot light's cone fade times its windowed distance falloff, in [0, 1].
// Mirrored on the CPU by `SpotLight::cone_and_range_at` in `light.rs`.
fn spot_attenuation(light: SpotLight, l: vec3<f32>, distance: f32) -> f32 {
    let cos_angle = dot(-l, light.direction);
    let cone = smoothstep(light.cos_outer, light.cos_inner, cos_angle);
    if light.range <= 0.0 {
        return 0.0;
    }
    let ratio = pow(distance / light.range, 4.0);
    let window = clamp(1.0 - ratio, 0.0, 1.0);
    return cone * window * window;
}

// Direct light from every scene light (the sun and up to
// `MAX_SHADOWED_LIGHTS` point/spot lights shadowed) plus hemisphere ambient
// scaled by `occlusion`, for a surface
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
            contribution *= sun_shadow(world_position);
        }
        direct_light += contribution;
    }

    for (var i = 0u; i < lights.point_count; i++) {
        let light = lights.point_lights[i];
        let to_light = light.position - world_position;
        let distance = max(length(to_light), 0.0001);
        let l = to_light / distance;
        let attenuation = 1.0 / (distance * distance);
        var radiance = light.color * light.intensity * attenuation;
        if light.shadow_slot >= 0 {
            let layer = u32(light.shadow_slot) * LAYERS_PER_LIGHT + point_face(-to_light);
            radiance *= local_shadow(layer, world_position);
        }
        direct_light += shade_light(l, radiance, n, v, albedo, metallic, roughness, f0);
    }

    for (var i = 0u; i < lights.spot_count; i++) {
        let light = lights.spot_lights[i];
        let to_light = light.position - world_position;
        let distance = max(length(to_light), 0.0001);
        let l = to_light / distance;
        let attenuation = spot_attenuation(light, l, distance) / (distance * distance);
        var radiance = light.color * light.intensity * attenuation;
        if light.shadow_slot >= 0 && attenuation > 0.0 {
            radiance *= local_shadow(u32(light.shadow_slot) * LAYERS_PER_LIGHT, world_position);
        }
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

// How much of the colour at `world_position` is fog, in [0, 1]: distance
// fog past the start distance plus height fog integrated along the view
// ray. Mirrored on the CPU by `Fog::amount` in `sky.rs` (and copied into
// `water.wgsl`); keep them in step.
fn fog_amount(world_position: vec3<f32>) -> f32 {
    let eye = camera.view_position.xyz;
    let distance = length(world_position - eye);
    let past_start = max(distance - camera.fog_height.x, 0.0);
    var optical = camera.fog_color_density.w * past_start;
    let height_density = camera.fog_height.y;
    if height_density > 0.0 && distance > 0.0 {
        let falloff = max(camera.fog_height.w, 0.0001);
        let start_density = exp(clamp(-falloff * (eye.y - camera.fog_height.z), -20.0, 20.0));
        let rise = falloff * (world_position.y - eye.y);
        var spread = 1.0;
        if abs(rise) >= 0.0001 {
            spread = (1.0 - exp(clamp(-rise, -20.0, 20.0))) / rise;
        }
        optical += height_density * start_density * spread * past_start;
    }
    return clamp(1.0 - exp(-optical), 0.0, 1.0);
}

// `color` seen through the fog at `world_position`.
fn apply_fog(color: vec3<f32>, world_position: vec3<f32>) -> vec3<f32> {
    return mix(color, camera.fog_color_density.rgb, fog_amount(world_position));
}
