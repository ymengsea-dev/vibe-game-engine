// Terrain fragment stage: blends up to eight ground textures by the
// tile's paint weights, then shades with `shade_surface`. Appended to
// `pbr_shared.wgsl`, followed by `pbr_vs.wgsl` for the vertex stage.
//
// `in.uv` runs 0 to 1 across the tile, landing on height nodes; paint has
// one texel per node, so the texel centres are half a texel in from the
// edges. Layer textures tile in world space, `tiling` units per copy.

struct TerrainUniform {
    // Linear RGB tint per layer; `w` unused.
    tints: array<vec4<f32>, 8>,
    // World units per texture copy, four layers per vector.
    tiling: array<vec4<f32>, 2>,
    layer_count: u32,
    splat_resolution: f32,
    roughness: f32,
    _padding: f32,
};

@group(1) @binding(0)
var splat0: texture_2d<f32>;
@group(1) @binding(1)
var splat1: texture_2d<f32>;
@group(1) @binding(2)
var layers: texture_2d_array<f32>;
@group(1) @binding(3)
var layer_sampler: sampler;
@group(1) @binding(4)
var splat_sampler: sampler;
@group(1) @binding(5)
var<uniform> terrain: TerrainUniform;

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let resolution = max(terrain.splat_resolution, 2.0);
    let splat_uv = (in.uv * (resolution - 1.0) + 0.5) / resolution;
    let first = textureSample(splat0, splat_sampler, splat_uv);
    let second = textureSample(splat1, splat_sampler, splat_uv);
    var weights = array<f32, 8>(
        first.x, first.y, first.z, first.w,
        second.x, second.y, second.z, second.w,
    );

    // Every layer is sampled (a fixed loop keeps the derivatives valid);
    // layers past `layer_count` weigh nothing.
    var albedo = vec3<f32>(0.0);
    var total = 0.0;
    for (var i = 0u; i < 8u; i++) {
        let tiling = max(terrain.tiling[i / 4u][i % 4u], 0.001);
        let colour = textureSample(layers, layer_sampler, in.world_position.xz / tiling, i32(i)).rgb;
        let weight = select(0.0, weights[i], i < terrain.layer_count);
        albedo += colour * terrain.tints[i].rgb * weight;
        total += weight;
    }
    // Sampled before the branch: texture sampling must stay in uniform
    // control flow.
    let fallback = textureSample(layers, layer_sampler, in.world_position.xz, 0).rgb;
    if total > 0.0001 {
        albedo /= total;
    } else {
        albedo = fallback;
    }

    let n = normalize(in.world_normal);
    let lit = shade_surface(albedo, 0.0, clamp(terrain.roughness, 0.045, 1.0), n, in.world_position, 1.0);
    return vec4<f32>(apply_fog(lit, in.world_position), 1.0);
}
