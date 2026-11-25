// The shader graph material: the same lighting as `pbr_material_fs.wgsl`,
// but the surface (base colour, metallic, roughness, normal, glow, alpha)
// and a vertex offset come from a graph compiled to WGSL by
// `engine_scene::shader_graph`. Built by `GpuContext::graph_pipelines` as
// `pbr_shared.wgsl` + this file + the graph's helpers and functions
// (`material_surface`, `material_vertex_offset`). Only unskinned meshes
// use it; shadows, skinned, instanced and vegetation draws keep the
// standard material stage.

@group(1) @binding(0)
var base_color_texture: texture_2d<f32>;
@group(1) @binding(1)
var base_color_sampler: sampler;

// Same layout as `pbr_material_fs.wgsl`'s; the graph reads only the
// texture repeat and the alpha settings.
struct MaterialUniform {
    base_color_factor: vec4<f32>,
    metallic_factor: f32,
    roughness_factor: f32,
    alpha_cutoff: f32,
    alpha_mode: u32,
    emissive_factor: vec3<f32>,
    normal_scale: f32,
    occlusion_strength: f32,
    uv_transform: vec4<f32>,
};

@group(1) @binding(2)
var<uniform> material: MaterialUniform;
@group(1) @binding(3)
var normal_map: texture_2d<f32>;
@group(1) @binding(4)
var emissive_map: texture_2d<f32>;
@group(1) @binding(5)
var occlusion_map: texture_2d<f32>;
@group(1) @binding(6)
var metallic_roughness_map: texture_2d<f32>;

// `values[0].x` is the time in seconds; `values[1..]` are the graph's
// parameters in the order `ShaderGraphData::parameters` gives.
struct GraphParams {
    values: array<vec4<f32>, 8>,
};

@group(1) @binding(7)
var<uniform> graph_params: GraphParams;

// What a graph's nodes read.
struct SurfaceInput {
    uv: vec2<f32>,
    world_position: vec3<f32>,
    normal: vec3<f32>,
    view_direction: vec3<f32>,
    time: f32,
};

// What a graph's Surface node gives back.
struct SurfaceOutput {
    base_color: vec3<f32>,
    metallic: f32,
    roughness: f32,
    // Tangent space: (0, 0, 1) leaves the surface flat.
    normal: vec3<f32>,
    emissive: vec3<f32>,
    alpha: f32,
};

struct ModelUniform {
    model: mat4x4<f32>,
    params: vec4<f32>,
};

@group(2) @binding(0)
var<uniform> object: ModelUniform;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
};

fn material_uv(uv: vec2<f32>) -> vec2<f32> {
    return uv * material.uv_transform.xy + material.uv_transform.zw;
}

// Bends `n` by a tangent-space normal, with the tangent basis from
// screen-space derivatives (see `apply_normal_map` in
// `pbr_material_fs.wgsl`). Derivatives are taken first so every pixel of
// a quad runs them.
fn apply_surface_normal(n: vec3<f32>, world_position: vec3<f32>, uv: vec2<f32>, tangent_normal: vec3<f32>) -> vec3<f32> {
    let dp1 = dpdx(world_position);
    let dp2 = dpdy(world_position);
    let duv1 = dpdx(uv);
    let duv2 = dpdy(uv);
    if abs(tangent_normal.x) < 1e-4 && abs(tangent_normal.y) < 1e-4 {
        return n;
    }
    let determinant = duv1.x * duv2.y - duv2.x * duv1.y;
    if abs(determinant) < 1e-8 {
        return n;
    }
    let inv = 1.0 / determinant;
    let tangent = graph_normalise3((dp1 * duv2.y - dp2 * duv1.y) * inv);
    let bitangent = graph_normalise3((dp2 * duv1.x - dp1 * duv2.x) * inv);
    let bent = graph_normalise3(tangent * tangent_normal.x + bitangent * tangent_normal.y + n * tangent_normal.z);
    if dot(bent, bent) < 0.5 {
        return n;
    }
    return bent;
}

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    let world_position = (object.model * vec4<f32>(in.position, 1.0)).xyz;
    let world_normal = (object.model * vec4<f32>(in.normal, 0.0)).xyz;
    var input: SurfaceInput;
    input.uv = material_uv(in.uv);
    input.world_position = world_position;
    input.normal = graph_normalise3(world_normal);
    input.view_direction = graph_normalise3(camera.view_position.xyz - world_position);
    input.time = graph_params.values[0].x;
    let moved = world_position + material_vertex_offset(input);
    out.clip_position = camera.view_proj * vec4<f32>(moved, 1.0);
    out.world_position = moved;
    out.world_normal = world_normal;
    out.uv = in.uv;
    out.fade = object.params.x;
    return out;
}

@fragment
fn fs_main(in: VertexOutput, @builtin(front_facing) front_facing: bool) -> @location(0) vec4<f32> {
    if in.fade < dither_threshold(in.clip_position.xy) {
        discard;
    }
    let uv = material_uv(in.uv);
    var n = graph_normalise3(in.world_normal);
    if !front_facing {
        n = -n;
    }
    var input: SurfaceInput;
    input.uv = uv;
    input.world_position = in.world_position;
    input.normal = n;
    input.view_direction = graph_normalise3(camera.view_position.xyz - in.world_position);
    input.time = graph_params.values[0].x;
    let surface = material_surface(input);
    let shaded_normal = apply_surface_normal(n, in.world_position, uv, surface.normal);

    // Mode 1 is `AlphaMode::Mask`, 2 `AlphaMode::Blend`.
    if material.alpha_mode == 1u && surface.alpha < material.alpha_cutoff {
        discard;
    }
    let lit = shade_surface(
        max(surface.base_color, vec3<f32>(0.0)),
        clamp(surface.metallic, 0.0, 1.0),
        clamp(surface.roughness, 0.045, 1.0),
        shaded_normal,
        in.world_position,
        1.0,
    );
    var out_alpha = 1.0;
    if material.alpha_mode == 2u {
        out_alpha = clamp(surface.alpha, 0.0, 1.0);
    }
    return vec4<f32>(apply_fog(lit + max(surface.emissive, vec3<f32>(0.0)), in.world_position), out_alpha);
}
