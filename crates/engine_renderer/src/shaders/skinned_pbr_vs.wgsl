// GPU-skinned vertex stage for the PBR shader. Concatenated onto
// `pbr_common.wgsl` (which declares `camera` and `VertexOutput`) by
// `GpuContext::create_skinned_pbr_pipeline`.
//
// Each vertex names up to four joints and four blend weights; the vertex
// is transformed by the weighted sum of those joints' skinning matrices
// (pose-composed global transform times inverse bind matrix, computed on
// the CPU by `engine_animation::compute_skinning_matrices`) before the
// per-object model matrix is applied.

struct ModelUniform {
    model: mat4x4<f32>,
};

@group(2) @binding(0)
var<uniform> object: ModelUniform;

// MAX_JOINTS: keep in sync with `engine_renderer::MAX_JOINTS`.
const MAX_JOINTS: u32 = 64u;

struct JointMatrices {
    // Number of valid entries in `matrices`; the rest are identity
    // padding. Unused by the shader itself (out-of-range joint indices
    // are a CPU-side bug) but kept so the buffer's contents are
    // self-describing.
    count: u32,
    matrices: array<mat4x4<f32>, MAX_JOINTS>,
};

@group(2) @binding(1)
var<uniform> skin: JointMatrices;

struct SkinnedVertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) joints: vec4<u32>,
    @location(4) weights: vec4<f32>,
};

@vertex
fn vs_main(in: SkinnedVertexInput) -> VertexOutput {
    var out: VertexOutput;

    // Weighted blend of the four influencing joints. glTF guarantees the
    // weights sum to ~1, so no renormalization here.
    let skin_matrix =
        in.weights.x * skin.matrices[in.joints.x]
        + in.weights.y * skin.matrices[in.joints.y]
        + in.weights.z * skin.matrices[in.joints.z]
        + in.weights.w * skin.matrices[in.joints.w];

    let skinned_position = skin_matrix * vec4<f32>(in.position, 1.0);
    let world_position = object.model * skinned_position;
    out.clip_position = camera.view_proj * world_position;
    out.world_position = world_position.xyz;
    // Same simplification as the unskinned path: no dedicated normal
    // matrix, so non-uniform scale in either the skin or model matrix
    // would skew normals. Fine for the rigid-ish joints skinning
    // produces in practice.
    out.world_normal = (object.model * skin_matrix * vec4<f32>(in.normal, 0.0)).xyz;
    out.uv = in.uv;
    return out;
}
