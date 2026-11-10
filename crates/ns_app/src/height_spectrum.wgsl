// Frankot–Chellappa integration in the frequency domain.
//
// The complex FFT of p + i·q contains both gradient spectra. Recover P and Q
// from conjugate symmetry, solve H = (-i·wx·P - i·wy·Q) / (wx² + wy²), and
// write the result bit-reversed on both axes for the inverse FFT.

struct Params {
    size_x: u32,
    size_y: u32,
    bits_x: u32,
    bits_y: u32,
    high_pass: f32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
};

@group(0) @binding(0) var<storage, read> src: array<vec2<f32>>;
@group(0) @binding(1) var<storage, read_write> dst: array<vec2<f32>>;
var<immediate> params: Params;

fn omega(k: u32, n: u32) -> f32 {
    let signed_index = select(i32(k), i32(k) - i32(n), k > n / 2u);
    return 6.28318530718 * f32(signed_index) / f32(n);
}

@compute @workgroup_size(256)
fn spectrum(@builtin(global_invocation_id) id: vec3<u32>) {
    let total = params.size_x * params.size_y;
    let index = id.x;
    if (index >= total) {
        return;
    }
    let x = index % params.size_x;
    let y = index / params.size_x;
    let nx = (params.size_x - x) % params.size_x;
    let ny = (params.size_y - y) % params.size_y;
    let f = src[index];
    let f_neg = src[ny * params.size_x + nx];

    // P = (F(k) + conj(F(-k))) / 2, Q = (F(k) - conj(F(-k))) / 2i
    let p = 0.5 * (f + vec2<f32>(f_neg.x, -f_neg.y));
    let q = vec2<f32>(0.5 * (f.y + f_neg.y), -0.5 * (f.x - f_neg.x));

    let wx = omega(x, params.size_x);
    let wy = omega(y, params.size_y);
    let denom = wx * wx + wy * wy;
    var h = vec2<f32>(0.0, 0.0);
    if (denom > 1e-12) {
        h = (wx * vec2<f32>(p.y, -p.x) + wy * vec2<f32>(q.y, -q.x)) / denom;
        let cutoff = params.high_pass * 6.28318530718;
        if (cutoff > 0.0) {
            h = h * (denom / (denom + cutoff * cutoff));
        }
    }

    let bx = reverseBits(x) >> (32u - params.bits_x);
    let by = reverseBits(y) >> (32u - params.bits_y);
    dst[by * params.size_x + bx] = h;
}
