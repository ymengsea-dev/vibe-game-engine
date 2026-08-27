// Vertex + fragment shader: transforms vertices by the camera's
// view-projection matrix, samples a base color texture, and shades with
// a physically-based (Cook-Torrance GGX) BRDF driven by the material's
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
};

@group(1) @binding(2)
var<uniform> material: MaterialUniform;

struct ModelUniform {
    model: mat4x4<f32>,
};

@group(2) @binding(0)
var<uniform> object: ModelUniform;

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

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_normal: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) world_position: vec3<f32>,
};

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    let world_position = object.model * vec4<f32>(in.position, 1.0);
    out.clip_position = camera.view_proj * world_position;
    out.world_position = world_position.xyz;
    // Normals ignore translation/scale by design here (no normal matrix
    // yet — a concern once non-uniform scale matters).
    out.world_normal = (object.model * vec4<f32>(in.normal, 0.0)).xyz;
    out.uv = in.uv;
    return out;
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

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let tex_color = textureSample(base_color_texture, base_color_sampler, in.uv);
    let base_color = tex_color * material.base_color_factor;
    let albedo = base_color.rgb;
    let metallic = clamp(material.metallic_factor, 0.0, 1.0);
    let roughness = clamp(material.roughness_factor, 0.045, 1.0);

    let n = normalize(in.world_normal);
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

    // Small constant ambient term so unlit faces aren't pure black —
    // real indirect lighting (IBL) is future work, not this feature.
    let ambient = albedo * 0.03;

    return vec4<f32>(ambient + direct_light, base_color.a);
}
