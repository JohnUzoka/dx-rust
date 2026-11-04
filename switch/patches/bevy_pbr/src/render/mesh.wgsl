#import bevy_pbr::{
    mesh_bindings::mesh,
    mesh_functions,
    mesh_view_bindings::view,
    skinning,
    morph::{morph_position, morph_normal, morph_tangent},
    forward_io::{Vertex, VertexOutput},
    view_transformations::position_world_to_clip,
}

// Switch build patch: DX RUST's first-person weapon, drawn by the world camera instead of a camera of its own. Its
// parts carry a `MeshTag` with `VIEW_MODEL_TAG_HIGH` in the high 16 bits (material.rs) and stay in their own camera's
// space, where the game puts them: that camera at the origin, looking down -Z. Each is projected as that camera would
// (the low 16 bits are its focal factor, 1 / tan(half its vertical field of view), in 4096ths; its near plane 1 cm;
// Bevy's infinite reversed Z) at the view's aspect ratio, with its depth moved into [0.9, 1]: in front of anything the
// world camera (near plane 5 cm) draws beyond 5.6 cm. Its world position, normal and tangent are put where that
// camera would see them in front of this view, for the fragment shader's fog and lights. Drawn by its own camera
// (at the origin, unturned), the same sums give that camera's picture.
const VIEW_MODEL_TAG_HIGH: u32 = 0x5649u;
const VIEW_MODEL_NEAR: f32 = 0.01;
const VIEW_MODEL_DEPTH_FLOOR: f32 = 0.9;

fn view_model_clip(p: vec3<f32>, focal: f32) -> vec4<f32> {
    let w = -p.z;
    // A perspective clip_from_view has focal / aspect at [0][0] and focal at [1][1].
    let x_scale = focal * view.clip_from_view[0][0] / view.clip_from_view[1][1];
    return vec4<f32>(
        p.x * x_scale,
        p.y * focal,
        VIEW_MODEL_DEPTH_FLOOR * w + (1.0 - VIEW_MODEL_DEPTH_FLOOR) * VIEW_MODEL_NEAR,
        w,
    );
}

#ifdef MORPH_TARGETS
// The instance_index parameter must match vertex_in.instance_index. This is a work around for a wgpu dx12 bug.
// See https://github.com/gfx-rs/naga/issues/2416
fn morph_vertex(vertex_in: Vertex, instance_index: u32) -> Vertex {
    var vertex = vertex_in;
    let first_vertex = mesh[instance_index].first_vertex_index;
    let vertex_index = vertex.index - first_vertex;

    let weight_count = bevy_pbr::morph::layer_count(instance_index);
    for (var i: u32 = 0u; i < weight_count; i ++) {
        let weight = bevy_pbr::morph::weight_at(i, instance_index);
        if weight == 0.0 {
            continue;
        }
        vertex.position += weight * morph_position(vertex_index, i, instance_index);
#ifdef VERTEX_NORMALS
        vertex.normal += weight * morph_normal(vertex_index, i, instance_index);
#endif
#ifdef VERTEX_TANGENTS
        vertex.tangent += vec4(weight * morph_tangent(vertex_index, i, instance_index), 0.0);
#endif
    }
    return vertex;
}
#endif

@vertex
fn vertex(vertex_no_morph: Vertex) -> VertexOutput {
    var out: VertexOutput;

#ifdef MORPH_TARGETS
    var vertex = morph_vertex(vertex_no_morph, vertex_no_morph.instance_index);
#else
    var vertex = vertex_no_morph;
#endif

    let mesh_world_from_local = mesh_functions::get_world_from_local(vertex_no_morph.instance_index);

#ifdef SKINNED
    // Use vertex_no_morph.instance_index instead of vertex.instance_index to work around a wgpu dx12 bug.
    // See https://github.com/gfx-rs/naga/issues/2416 .
    var world_from_local = skinning::skin_model(
        vertex.joint_indices,
        vertex.joint_weights,
        vertex_no_morph.instance_index
    );
#else
    var world_from_local = mesh_world_from_local;
#endif

#ifdef VERTEX_NORMALS
#ifdef SKINNED
    out.world_normal = skinning::skin_normals(world_from_local, vertex.normal);
#else
    out.world_normal = mesh_functions::mesh_normal_local_to_world(
        vertex.normal,
        // Use vertex_no_morph.instance_index instead of vertex.instance_index to work around a wgpu dx12 bug.
        // See https://github.com/gfx-rs/naga/issues/2416
        vertex_no_morph.instance_index
    );
#endif
#endif

#ifdef VERTEX_POSITIONS
    out.world_position = mesh_functions::mesh_position_local_to_world(world_from_local, vec4<f32>(vertex.position, 1.0));
    out.position = position_world_to_clip(out.world_position.xyz);
#endif

#ifdef VERTEX_UVS_A
    out.uv = vertex.uv;
#endif
#ifdef VERTEX_UVS_B
    out.uv_b = vertex.uv_b;
#endif

#ifdef VERTEX_TANGENTS
    out.world_tangent = mesh_functions::mesh_tangent_local_to_world(
        world_from_local,
        vertex.tangent,
        // Use vertex_no_morph.instance_index instead of vertex.instance_index to work around a wgpu dx12 bug.
        // See https://github.com/gfx-rs/naga/issues/2416
        vertex_no_morph.instance_index
    );
#endif

#ifdef VERTEX_COLORS
    out.color = vertex.color;
#endif

#ifdef VERTEX_OUTPUT_INSTANCE_INDEX
    // Use vertex_no_morph.instance_index instead of vertex.instance_index to work around a wgpu dx12 bug.
    // See https://github.com/gfx-rs/naga/issues/2416
    out.instance_index = vertex_no_morph.instance_index;
#endif

#ifdef VISIBILITY_RANGE_DITHER
    out.visibility_range_dither = mesh_functions::get_visibility_range_dither_level(
        vertex_no_morph.instance_index, mesh_world_from_local[3]);
#endif

    // Switch build patch: a weapon part (top of file).
    let tag = mesh_functions::get_tag(vertex_no_morph.instance_index);
    if (tag >> 16u) == VIEW_MODEL_TAG_HIGH {
#ifdef VERTEX_POSITIONS
        let p = out.world_position.xyz;
        out.position = view_model_clip(p, f32(tag & 0xFFFFu) / 4096.0);
        out.world_position = vec4<f32>(view.world_position + (view.world_from_view * vec4<f32>(p, 0.0)).xyz, 1.0);
#endif
#ifdef VERTEX_NORMALS
        out.world_normal = (view.world_from_view * vec4<f32>(out.world_normal, 0.0)).xyz;
#endif
#ifdef VERTEX_TANGENTS
        out.world_tangent = vec4<f32>(
            (view.world_from_view * vec4<f32>(out.world_tangent.xyz, 0.0)).xyz, out.world_tangent.w);
#endif
    }

    return out;
}

@fragment
fn fragment(
    mesh: VertexOutput,
) -> @location(0) vec4<f32> {
#ifdef VERTEX_COLORS
    return mesh.color;
#else
    return vec4<f32>(1.0, 0.0, 1.0, 1.0);
#endif
}
