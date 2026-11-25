// Skybox: a full-screen triangle (no vertex/index buffer) painted with a
// horizon-to-zenith gradient (or an equirectangular sky image), drifting
// noise clouds and a glow toward the sun, keyed entirely off a per-pixel
// view ray reconstructed from the inverse view-projection matrix. Drawn
// first in the main color pass, before any scene geometry, so opaque
// objects paint over it. Mirrors `SkyboxUniform` in `skybox.rs`.

struct SkyboxUniform {
    inverse_view_proj: mat4x4<f32>,
    camera_position: vec4<f32>,
    // xyz = direction the sun travels *toward* (not toward the source).
    sun_direction: vec4<f32>,
    sun_color: vec4<f32>,
    zenith: vec4<f32>,
    horizon: vec4<f32>,
    // xyz = colour below the horizon, w = how much it shows.
    ground: vec4<f32>,
    // coverage, speed, time, 1 when there are clouds.
    clouds: vec4<f32>,
    cloud_color: vec4<f32>,
    // xyz = fog colour, w = how much it veils the horizon.
    fog: vec4<f32>,
    // x = 1: sample the sky image.
    mode: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> sky: SkyboxUniform;
@group(0) @binding(1)
var sky_image: texture_2d<f32>;
@group(0) @binding(2)
var sky_sampler: sampler;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) ndc: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOutput {
    // Oversized triangle covering the whole viewport in one draw — the
    // standard "full-screen triangle" trick, cheaper than a quad (no
    // diagonal seam, no index buffer).
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    let pos = positions[index];

    var out: VertexOutput;
    // z=1, w=1: the far plane in wgpu's [0,1] depth range — correct for
    // "infinitely far background" even though this pipeline has no depth
    // attachment to test it against.
    out.clip_position = vec4<f32>(pos, 1.0, 1.0);
    out.ndc = pos;
    return out;
}

const PI: f32 = 3.14159265359;

fn hash(cell: vec2<f32>) -> f32 {
    return fract(sin(dot(cell, vec2<f32>(127.1, 311.7))) * 43758.5453);
}

// Smooth value noise in [0, 1].
fn value_noise(p: vec2<f32>) -> f32 {
    let cell = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let a = hash(cell);
    let b = hash(cell + vec2<f32>(1.0, 0.0));
    let c = hash(cell + vec2<f32>(0.0, 1.0));
    let d = hash(cell + vec2<f32>(1.0, 1.0));
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

// Four octaves of noise: big puffy shapes with ragged edges.
fn fbm(p: vec2<f32>) -> f32 {
    var total = 0.0;
    var amplitude = 0.5;
    var q = p;
    for (var octave = 0; octave < 4; octave++) {
        total += value_noise(q) * amplitude;
        q = q * 2.03 + vec2<f32>(17.0, 9.0);
        amplitude *= 0.5;
    }
    return total;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Unproject this pixel's far-plane clip-space point back to world
    // space, then subtract the camera position to get the view ray.
    let far_point_clip = vec4<f32>(in.ndc, 1.0, 1.0);
    let far_point_world = sky.inverse_view_proj * far_point_clip;
    let world_point = far_point_world.xyz / far_point_world.w;
    let ray = normalize(world_point - sky.camera_position.xyz);
    let to_sun = normalize(-sky.sun_direction.xyz);

    // Sampled outside any branch: texture sampling must stay in uniform
    // control flow. Equirectangular: longitude across, latitude down.
    let image_uv = vec2<f32>(
        atan2(ray.x, -ray.z) / (2.0 * PI) + 0.5,
        acos(clamp(ray.y, -1.0, 1.0)) / PI,
    );
    let image_color = textureSampleLevel(sky_image, sky_sampler, image_uv, 0.0).rgb;

    var color: vec3<f32>;
    if sky.mode.x > 0.5 {
        color = image_color;
    } else {
        let height = clamp(ray.y * 0.5 + 0.5, 0.0, 1.0);
        color = mix(sky.horizon.rgb, sky.zenith.rgb, height);
        // Below the horizon, fade toward the ground colour.
        let below = clamp(-ray.y * 4.0, 0.0, 1.0) * sky.ground.w;
        color = mix(color, sky.ground.rgb, below);

        if sky.clouds.w > 0.5 && ray.y > 0.0 {
            // A flat cloud layer above the camera, seen along the ray.
            let plane = ray.xz / max(ray.y, 0.05);
            let drift = vec2<f32>(1.0, 0.35) * sky.clouds.y * sky.clouds.z * 0.02;
            let density = fbm(plane * 0.6 + drift);
            let coverage = sky.clouds.x;
            let cloud = smoothstep(1.0 - coverage, 1.0 - coverage * 0.5, density);
            // Thin out toward the horizon, where the layer is far away.
            let fade = smoothstep(0.02, 0.25, ray.y);
            let lit = sky.cloud_color.rgb * (0.65 + 0.35 * max(dot(to_sun, vec3<f32>(0.0, 1.0, 0.0)), 0.0));
            color = mix(color, lit, cloud * fade);
        }
    }

    // Fog veils the horizon in its own colour.
    let horizon_veil = (1.0 - smoothstep(0.0, 0.35, abs(ray.y))) * sky.fog.w;
    color = mix(color, sky.fog.rgb, horizon_veil);

    // Sun glow: brightest looking toward the light source (opposite its
    // travel direction), falling off sharply with angle.
    let sun_amount = pow(max(dot(ray, to_sun), 0.0), 256.0);
    color += sky.sun_color.rgb * sun_amount * 4.0;

    return vec4<f32>(color, 1.0);
}
