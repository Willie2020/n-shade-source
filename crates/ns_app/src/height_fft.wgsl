// Out-of-place radix-2 FFT passes for GPU height integration.
//
// `prep` leaves the X axis bit-reversed. Each pass doubles the transform size,
// reading from one buffer and writing to the other. `bitrev` reorders Y before
// the column passes. `inverse` selects the sign of the twiddle factor.

struct Params {
    size_x: u32,
    size_y: u32,
    stage: u32,
    inverse: u32,
    axis: u32,
    bits: u32,
    _pad0: u32,
    _pad1: u32,
};

@group(0) @binding(0) var<storage, read> src: array<vec2<f32>>;
@group(0) @binding(1) var<storage, read_write> dst: array<vec2<f32>>;
var<immediate> params: Params;

fn cmul(a: vec2<f32>, b: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(a.x * b.x - a.y * b.y, a.x * b.y + a.y * b.x);
}

@compute @workgroup_size(256)
fn fft(@builtin(global_invocation_id) id: vec3<u32>) {
    let pairs = (params.size_x * params.size_y) / 2u;
    let index = id.x;
    if (index >= pairs) {
        return;
    }

    let m = 1u << params.stage;
    let span = m * 2u;
    var i0 = 0u;
    var i1 = 0u;
    var j = 0u;

    if (params.axis == 0u) {
        let pairs_x = params.size_x / 2u;
        let row = index / pairs_x;
        let pair = index % pairs_x;
        let group = pair / m;
        j = pair % m;
        i0 = row * params.size_x + group * span + j;
        i1 = i0 + m;
    } else {
        let pairs_y = params.size_y / 2u;
        let column = index / pairs_y;
        let pair = index % pairs_y;
        let group = pair / m;
        j = pair % m;
        i0 = (group * span + j) * params.size_x + column;
        i1 = i0 + m * params.size_x;
    }

    var angle = -6.28318530718 * f32(j) / f32(span);
    if (params.inverse != 0u) {
        angle = -angle;
    }
    let w = vec2<f32>(cos(angle), sin(angle));
    let a = src[i0];
    let b = cmul(src[i1], w);
    dst[i0] = a + b;
    dst[i1] = a - b;
}

@compute @workgroup_size(256)
fn bitrev(@builtin(global_invocation_id) id: vec3<u32>) {
    let total = params.size_x * params.size_y;
    let index = id.x;
    if (index >= total) {
        return;
    }
    let x = index % params.size_x;
    let y = index / params.size_x;
    let ry = reverseBits(y) >> (32u - params.bits);
    dst[ry * params.size_x + x] = src[index];
}
