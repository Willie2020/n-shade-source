// Normalize the integrated height to 0–1 and write the texture consumed by
// the 2D canvas and the preview material.

struct Params {
    width: u32,
    height: u32,
    stride: u32,
    _pad: u32,
};

@group(0) @binding(0) var<storage, read> raw: array<f32>;
@group(0) @binding(1) var<storage, read> range: array<u32>;
@group(0) @binding(2) var height_out: texture_storage_2d<rgba8unorm, write>;
var<immediate> params: Params;

fn unsortable(key: u32) -> f32 {
    if ((key & 0x80000000u) != 0u) {
        return bitcast<f32>(key & 0x7fffffffu);
    }
    return bitcast<f32>(~key);
}

@compute @workgroup_size(256)
fn normalize(@builtin(global_invocation_id) id: vec3<u32>) {
    let total = params.width * params.height;
    let index = id.x;
    if (index >= total) {
        return;
    }
    let lo = unsortable(range[0]);
    let hi = unsortable(range[1]);
    let span = hi - lo;
    var value = 0.5;
    if (span > 1e-6 && span < 1e30) {
        value = clamp((raw[index] - lo) / span, 0.0, 1.0);
    }
    let coord = vec2<i32>(i32(index % params.width), i32(index / params.width));
    textureStore(height_out, coord, vec4<f32>(value, value, value, 1.0));
}
