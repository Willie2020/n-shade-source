// Crop the inverse FFT to the source rectangle, store raw heights, and gather
// the global min/max with atomics. Normalization is a separate pass so the
// range is complete before any texel is written.

struct Params {
    width: u32,
    height: u32,
    stride: u32,
    scale: f32,
};

@group(0) @binding(0) var<storage, read> src: array<vec2<f32>>;
@group(0) @binding(1) var<storage, read_write> raw: array<f32>;
@group(0) @binding(2) var<storage, read_write> range: array<atomic<u32>>;
var<immediate> params: Params;

fn sortable(f: f32) -> u32 {
    let bits = bitcast<u32>(f);
    if ((bits & 0x80000000u) != 0u) {
        return ~bits;
    }
    return bits | 0x80000000u;
}

@compute @workgroup_size(256)
fn finalize(@builtin(global_invocation_id) id: vec3<u32>) {
    let total = params.width * params.height;
    let index = id.x;
    if (index >= total) {
        return;
    }
    let x = index % params.width;
    let y = index / params.width;
    let h = src[y * params.stride + x].x * params.scale;
    raw[index] = h;
    if (h == h && abs(h) < 1e30) {
        let key = sortable(h);
        atomicMin(&range[0], key);
        atomicMax(&range[1], key);
    }
}
