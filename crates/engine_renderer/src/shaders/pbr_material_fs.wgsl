// The material fragment stage: samples a base color texture and shades
// with `shade_surface` (Cook-Torrance GGX, from `pbr_shared.wgsl`) driven
// by the material's metallic-roughness factors and optional normal,
// emissive and occlusion maps. Appended to `pbr_shared.wgsl`; together
// they are `PBR_COMMON_SOURCE`, shared by the unskinned (`pbr_vs.wgsl`),
// GPU-skinned (`skinned_pbr_vs.wgsl`), instanced and vegetation vertex
// stages.

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

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Distance fade: drop a growing share of pixels in a fixed pattern,
    // so far objects thin out instead of popping, without blending.
    if in.fade < dither_threshold(in.clip_position.xy) {
        discard;
    }

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

    let occlusion_sample = textureSample(occlusion_map, base_color_sampler, in.uv).r;
    let occlusion = mix(1.0, occlusion_sample, clamp(material.occlusion_strength, 0.0, 1.0));
    let lit = shade_surface(albedo, metallic, roughness, n, in.world_position, occlusion);

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
    return vec4<f32>(lit + emissive, out_alpha);
}
