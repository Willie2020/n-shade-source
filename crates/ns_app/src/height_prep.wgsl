// Prepare a normal map for GPU height integration.
//
// The normal field is converted to a gradient field p + iq = dh/dx + i·dh/dy,
// mirror-padded when the document uses clamped wrapping, and written with the
// X axis bit-reversed for the decimation-in-time FFT passes.

struct Params {
    src_w: u32,
    src_h: u32,
    pad_w: u32,
    pad_h: u32,
    repeat: u32,
    bits_x: u32,
    bits_y: u32,
    _pad: u32,
};

@group(0) @binding(0) var normal_in: texture_storage_2d<rgba8unorm, read>;
@group(0) @binding(1) var<storage, read_write> dst: array<vec2<f32>>;
var<immediate> params: Params;

fn mirrored(i: u32, n: u32) -> u32 {
    let period = 2u * n;
    let j = i % period;
    return select(j, period - 1u - j, j >= n);
}

@compute @workgroup_size(256)
fn prep(@builtin(global_invocation_id) id: vec3<u32>) {
    let total = params.pad_w * params.pad_h;
    let index = id.x;
    if (index >= total) {
        return;
    }
    let x = index % params.pad_w;
    let y = index / params.pad_w;

    var sx = x;
    var sy = y;
    var flip_x = false;
    var flip_y = false;
    if (params.repeat != 0u) {
        sx = x % params.src_w;
        sy = y % params.src_h;
    } else {
        sx = mirrored(x, params.src_w);
        sy = mirrored(y, params.src_h);
        flip_x = x >= params.src_w;
        flip_y = y >= params.src_h;
    }

    let encoded = textureLoad(normal_in, vec2<i32>(i32(sx), i32(sy)));
    var n = encoded.xyz * 2.0 - vec3<f32>(1.0);
    let len = length(n);
    if (len > 1e-5) {
        n = n / len;
    } else {
        n = vec3<f32>(0.0, 0.0, 1.0);
    }
    let nz = max(n.z, 1e-4);
    var dx = -n.x / nz;
    var dy = n.y / nz;
    if (flip_x) {
        dx = -dx;
    }
    if (flip_y) {
        dy = -dy;
    }

    let bx = reverseBits(x) >> (32u - params.bits_x);
    dst[y * params.pad_w + bx] = vec2<f32>(dx, dy);
}
