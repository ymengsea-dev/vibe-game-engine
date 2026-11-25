// Helpers shader graph code calls (see `engine_scene::shader_graph`):
// value noise, which `engine_scene::value_noise` mirrors on the CPU, and
// normalise that leaves a zero vector at zero instead of NaN.

fn graph_hash(p: vec3<f32>) -> f32 {
    let q = fract(p * 0.3183099 + vec3<f32>(0.1, 0.2, 0.3)) * 17.0;
    return fract(q.x * q.y * q.z * (q.x + q.y + q.z));
}

fn graph_noise(p: vec3<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let low = mix(
        mix(graph_hash(i), graph_hash(i + vec3<f32>(1.0, 0.0, 0.0)), u.x),
        mix(graph_hash(i + vec3<f32>(0.0, 1.0, 0.0)), graph_hash(i + vec3<f32>(1.0, 1.0, 0.0)), u.x),
        u.y,
    );
    let high = mix(
        mix(graph_hash(i + vec3<f32>(0.0, 0.0, 1.0)), graph_hash(i + vec3<f32>(1.0, 0.0, 1.0)), u.x),
        mix(graph_hash(i + vec3<f32>(0.0, 1.0, 1.0)), graph_hash(i + vec3<f32>(1.0, 1.0, 1.0)), u.x),
        u.y,
    );
    return mix(low, high, u.z);
}

fn graph_normalise2(v: vec2<f32>) -> vec2<f32> {
    let length_squared = dot(v, v);
    if length_squared < 1e-12 { return v; }
    return v * inverseSqrt(length_squared);
}

fn graph_normalise3(v: vec3<f32>) -> vec3<f32> {
    let length_squared = dot(v, v);
    if length_squared < 1e-12 { return v; }
    return v * inverseSqrt(length_squared);
}

fn graph_normalise4(v: vec4<f32>) -> vec4<f32> {
    let length_squared = dot(v, v);
    if length_squared < 1e-12 { return v; }
    return v * inverseSqrt(length_squared);
}
