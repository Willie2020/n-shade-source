// Vendor-neutral GPU path tracer for white lightmaps.
//
// One invocation per UV texel. The CPU rasterizer supplies a position/normal
// G-buffer, and the BVH is flattened into leaf-order triangle and node buffers.
// Samples are dispatched in small batches so a slow GPU cannot trigger a TDR
// while the bake is running.

struct Params {
    size: vec2<u32>,
    samples: u32,
    bounces: u32,
    sample_offset: u32,
    seed: u32,
    sun_enabled: u32,
    batch: u32,
    sun_dir: vec4<f32>,
    sun_color: vec4<f32>,
    env_params: vec4<f32>,
    fill: vec4<f32>,
    eps: f32,
    shadow_dist: f32,
    sun_cos_max: f32,
    /// 1 for ambient occlusion, 0 for a radiance bake.
    mode: u32,
    /// `x` is the number of scene lights.
    light_info: vec4<u32>,
    /// [v2] `x` flags, `y` global first sample, `z` total samples, `w` unused.
    v2_flags: vec4<u32>,
    /// [v2] `x` extent, `y` AO max distance (mesh units), `zw` unused.
    v2_misc: vec4<f32>,
};

struct GpuLight {
    /// `xyz` center, `w` kind (0 point, 1 spot, 2 area).
    position: vec4<f32>,
    /// `xyz` emission axis, `w` outer cone cosine.
    direction: vec4<f32>,
    /// `rgb` color × intensity, `w` inner cone cosine.
    color: vec4<f32>,
    /// Half width, half height, area, two-sided flag.
    size: vec4<f32>,
    /// `x` cutoff range.
    range: vec4<f32>,
};

struct GpuNode {
    bmin: vec4<f32>,
    bmax: vec4<f32>,
    links: vec4<u32>,
};

struct GpuTriangle {
    p0: vec4<f32>,
    p1: vec4<f32>,
    p2: vec4<f32>,
    n0: vec4<f32>,
    n1: vec4<f32>,
    n2: vec4<f32>,
};

struct Hit {
    valid: bool,
    t: f32,
    point: vec3<f32>,
    normal: vec3<f32>,
    face: vec3<f32>,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> triangles: array<GpuTriangle>;
@group(0) @binding(2) var<storage, read> nodes: array<GpuNode>;
@group(0) @binding(3) var<storage, read> gbuffer_pos: array<vec4<f32>>;
@group(0) @binding(4) var<storage, read> gbuffer_nrm: array<vec4<f32>>;
@group(0) @binding(5) var<storage, read_write> accum: array<vec4<f32>>;
@group(0) @binding(6) var env_tex: texture_2d<f32>;
@group(0) @binding(7) var<storage, read> env_rows: array<f32>;
@group(0) @binding(8) var<storage, read> env_cols: array<f32>;
@group(0) @binding(9) var<storage, read> lights: array<GpuLight>;

const PI: f32 = 3.141592653589793;
const TAU: f32 = 6.283185307179586;
const FAR: f32 = 1.0e20;

var<private> rng_state: u32;

fn pcg_hash(input: u32) -> u32 {
    let state = input * 747796405u + 2891336453u;
    let word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
    return (word >> 22u) ^ word;
}

fn rand_seed(index: u32, sample: u32) -> u32 {
    return pcg_hash(index ^ pcg_hash(params.seed ^ (sample * 0x9e3779b9u)));
}

fn rand_f32() -> f32 {
    rng_state = pcg_hash(rng_state);
    return f32(rng_state >> 8u) / 16777216.0;
}

// ---------------------------------------------------------------------------
// v2: Owen-scrambled Sobol sampler. Mirrors sampling.rs bit for bit.
// ---------------------------------------------------------------------------

fn hash_u32(x_in: u32) -> u32 {
    var x = x_in;
    x ^= x >> 16u;
    x *= 0x7feb352du;
    x ^= x >> 15u;
    x *= 0x846ca68bu;
    x ^= x >> 16u;
    return x;
}

fn laine_karras(x_in: u32, seed: u32) -> u32 {
    var x = x_in + seed;
    x ^= x * 0x6c50b47cu;
    x ^= x * 0xb82f1e52u;
    x ^= x * 0xc7afe638u;
    x ^= x * 0x8d22f6e6u;
    return x;
}

fn owen_scramble(x: u32, seed: u32) -> u32 {
    return reverseBits(laine_karras(reverseBits(x), seed));
}

fn sobol_dim1(i_in: u32) -> u32 {
    var i = i_in;
    var v = 0x80000000u;
    var r = 0u;
    loop {
        if (i == 0u) { break; }
        if ((i & 1u) != 0u) { r ^= v; }
        i >>= 1u;
        v ^= v >> 1u;
    }
    return r;
}

fn to_unit(x: u32) -> f32 {
    return f32(x >> 8u) * (1.0 / 16777216.0);
}

var<private> smp_seed: u32;
var<private> smp_texel: u32;
var<private> smp_index: u32;
var<private> smp_mask: u32;
var<private> smp_pair: u32;
var<private> fb_state: u32;

fn smp_init(seed: u32, texel: u32, index: u32, total: u32) {
    smp_seed = seed;
    smp_texel = texel;
    smp_index = index;
    smp_mask = (1u << firstLeadingBit(max(total, 1u) * 2u - 1u)) - 1u;
    smp_pair = 0u;
    fb_state = hash_u32(seed ^ hash_u32(texel)) ^ hash_u32(index ^ 0x5bd1e995u);
}

fn fb() -> f32 {
    fb_state = fb_state * 747796405u + 2891336453u;
    var w = ((fb_state >> ((fb_state >> 28u) + 4u)) ^ fb_state) * 277803737u;
    w = (w >> 22u) ^ w;
    return to_unit(w);
}

fn next_2d() -> vec2<f32> {
    let pair = smp_pair;
    smp_pair += 1u;
    if (pair >= 4u) {
        return vec2<f32>(fb(), fb());
    }
    let s = hash_u32(smp_seed ^ hash_u32(smp_texel) ^ (pair * 0x9e3779b9u));
    let i = owen_scramble(smp_index, s ^ 0x68bc21ebu) & smp_mask;
    let a = owen_scramble(reverseBits(i), hash_u32(s ^ 0x02e5be93u));
    let b = owen_scramble(sobol_dim1(i), hash_u32(s ^ 0x967a889bu));
    return vec2<f32>(to_unit(a), to_unit(b));
}

fn rnd1(v2_ld: bool) -> f32 {
    if (v2_ld) { return fb(); }
    return rand_f32();
}

fn rnd2(v2_ld: bool) -> vec2<f32> {
    if (v2_ld) { return next_2d(); }
    return vec2<f32>(rand_f32(), rand_f32());
}

// ---------------------------------------------------------------------------
// v2: direction mappings and geometric helpers
// ---------------------------------------------------------------------------

fn cosine_hemisphere(u: f32, v: f32) -> vec3<f32> {
    let r = sqrt(u);
    let phi = 2.0 * PI * v;
    return vec3<f32>(r * cos(phi), r * sin(phi), sqrt(max(0.0, 1.0 - u)));
}

fn uniform_cone(u: f32, v: f32, cos_max: f32) -> vec3<f32> {
    let cos_theta = 1.0 - u * (1.0 - cos_max);
    let sin_theta = sqrt(max(0.0, 1.0 - cos_theta * cos_theta));
    let phi = 2.0 * PI * v;
    return vec3<f32>(sin_theta * cos(phi), sin_theta * sin(phi), cos_theta);
}

fn orthonormal_basis(n: vec3<f32>) -> mat3x3<f32> {
    let s = select(-1.0, 1.0, n.z >= 0.0);
    let a = -1.0 / (s + n.z);
    let b = n.x * n.y * a;
    let tangent = vec3<f32>(1.0 + s * n.x * n.x * a, s * b, -s * n.x);
    let bitangent = vec3<f32>(b, s + n.y * n.y * a, -n.y);
    return mat3x3<f32>(tangent, bitangent, n);
}

fn to_world(local: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
    return normalize(orthonormal_basis(n) * local);
}

fn ray_eps(extent: f32, p: vec3<f32>) -> f32 {
    return extent * 1.0e-5 + max(abs(p.x), max(abs(p.y), abs(p.z))) * 4.0e-6;
}

fn sign_nz(v: f32) -> f32 {
    return select(-1.0, 1.0, v >= 0.0);
}

fn unpack_geo_normal(w: f32) -> vec3<f32> {
    let q = u32(w);
    let e = vec2<f32>(f32(q / 4096u), f32(q % 4096u)) / 4095.0 * 2.0 - 1.0;
    var n = vec3<f32>(e.x, e.y, 1.0 - abs(e.x) - abs(e.y));
    if (n.z < 0.0) {
        n = vec3<f32>(
            (1.0 - abs(e.y)) * sign_nz(e.x),
            (1.0 - abs(e.x)) * sign_nz(e.y),
            n.z,
        );
    }
    return normalize(n);
}

fn face_normal_toward(face: vec3<f32>, toward: vec3<f32>) -> vec3<f32> {
    if (dot(face, toward) < 0.0) { return -face; }
    return face;
}

fn basis_from_normal(n: vec3<f32>) -> mat3x3<f32> {
    var axis = vec3<f32>(1.0, 0.0, 0.0);
    if (abs(n.x) > 0.9) {
        axis = vec3<f32>(0.0, 1.0, 0.0);
    }
    let tangent = normalize(axis - n * dot(n, axis));
    let bitangent = cross(n, tangent);
    return mat3x3<f32>(tangent, bitangent, n);
}

fn cosine_sample(normal: vec3<f32>) -> vec3<f32> {
    let r1 = rand_f32();
    let r2 = rand_f32();
    let radius = sqrt(r1);
    let phi = r2 * TAU;
    let local = vec3<f32>(
        radius * cos(phi),
        radius * sin(phi),
        sqrt(max(0.0, 1.0 - r1)),
    );
    return normalize(basis_from_normal(normal) * local);
}

fn aabb_hit(
    bmin: vec3<f32>,
    bmax: vec3<f32>,
    origin: vec3<f32>,
    direction: vec3<f32>,
    tmax: f32,
) -> bool {
    var t0 = 0.0;
    var t1 = tmax;
    for (var axis = 0u; axis < 3u; axis += 1u) {
        let oa = origin[axis];
        let da = direction[axis];
        if (abs(da) < 1.0e-12) {
            if (oa < bmin[axis] || oa > bmax[axis]) {
                return false;
            }
        } else {
            let inverse = 1.0 / da;
            var ta = (bmin[axis] - oa) * inverse;
            var tb = (bmax[axis] - oa) * inverse;
            if (ta > tb) {
                let temporary = ta;
                ta = tb;
                tb = temporary;
            }
            t0 = max(t0, ta);
            t1 = min(t1, tb);
            if (t0 > t1) {
                return false;
            }
        }
    }
    return true;
}

fn intersect_triangle(
    triangle: GpuTriangle,
    origin: vec3<f32>,
    direction: vec3<f32>,
    tmax: f32,
) -> vec3<f32> {
    let p0 = triangle.p0.xyz;
    let p1 = triangle.p1.xyz;
    let p2 = triangle.p2.xyz;
    let edge1 = p1 - p0;
    let edge2 = p2 - p0;
    let pvec = cross(direction, edge2);
    let determinant = dot(edge1, pvec);
    if (abs(determinant) < 1.0e-10) {
        return vec3<f32>(-1.0);
    }
    let inverse = 1.0 / determinant;
    let tvec = origin - p0;
    let u = dot(tvec, pvec) * inverse;
    if (u < -1.0e-6 || u > 1.0 + 1.0e-6) {
        return vec3<f32>(-1.0);
    }
    let qvec = cross(tvec, edge1);
    let v = dot(direction, qvec) * inverse;
    if (v < -1.0e-6 || u + v > 1.0 + 1.0e-6) {
        return vec3<f32>(-1.0);
    }
    let t = dot(edge2, qvec) * inverse;
    if (t > 1.0e-7 && t < tmax) {
        return vec3<f32>(t, u, v);
    }
    return vec3<f32>(-1.0);
}

fn trace(origin: vec3<f32>, direction: vec3<f32>, tmax: f32) -> Hit {
    var hit: Hit;
    hit.valid = false;
    hit.t = tmax;
    hit.point = origin;
    hit.normal = vec3<f32>(0.0, 1.0, 0.0);
    hit.face = vec3<f32>(0.0, 1.0, 0.0);

    var stack: array<u32, 64>;
    stack[0] = 0u;
    var top = 1u;
    var best = tmax;
    var best_u = 0.0;
    var best_v = 0.0;
    var best_triangle = 0u;
    var found = false;

    while (top > 0u) {
        top -= 1u;
        let node = nodes[stack[top]];
        if (!aabb_hit(node.bmin.xyz, node.bmax.xyz, origin, direction, best)) {
            continue;
        }
        let links = node.links;
        if (links.w > 0u) {
            for (var index = links.z; index < links.z + links.w; index += 1u) {
                let result = intersect_triangle(triangles[index], origin, direction, best);
                if (result.x > 0.0) {
                    best = result.x;
                    best_u = result.y;
                    best_v = result.z;
                    best_triangle = index;
                    found = true;
                }
            }
        } else if (top + 2u <= 64u) {
            stack[top] = links.x;
            top += 1u;
            stack[top] = links.y;
            top += 1u;
        }
    }

    if (found) {
        let triangle = triangles[best_triangle];
        let interpolated = triangle.n0.xyz * (1.0 - best_u - best_v)
            + triangle.n1.xyz * best_u
            + triangle.n2.xyz * best_v;
        let face = normalize(cross(
            triangle.p1.xyz - triangle.p0.xyz,
            triangle.p2.xyz - triangle.p0.xyz,
        ));
        hit.valid = true;
        hit.t = best;
        hit.point = origin + direction * best;
        hit.face = face;
        hit.normal = normalize(select(face, interpolated, length(interpolated) > 1.0e-8));
    }
    return hit;
}

fn occluded(origin: vec3<f32>, direction: vec3<f32>, tmax: f32) -> bool {
    var stack: array<u32, 64>;
    stack[0] = 0u;
    var top = 1u;
    while (top > 0u) {
        top -= 1u;
        let node = nodes[stack[top]];
        if (!aabb_hit(node.bmin.xyz, node.bmax.xyz, origin, direction, tmax)) {
            continue;
        }
        let links = node.links;
        if (links.w > 0u) {
            for (var index = links.z; index < links.z + links.w; index += 1u) {
                if (intersect_triangle(triangles[index], origin, direction, tmax).x > 0.0) {
                    return true;
                }
            }
        } else if (top + 2u <= 64u) {
            stack[top] = links.x;
            top += 1u;
            stack[top] = links.y;
            top += 1u;
        }
    }
    return false;
}

fn wrap_x(x: i32, width: i32) -> i32 {
    return ((x % width) + width) % width;
}

fn clamp_y(y: i32, height: i32) -> i32 {
    return clamp(y, 0, height - 1);
}

fn env_texel_at(x: i32, y: i32) -> vec3<f32> {
    let dimensions = textureDimensions(env_tex);
    let width = i32(dimensions.x);
    let height = i32(dimensions.y);
    return textureLoad(env_tex, vec2<i32>(wrap_x(x, width), clamp_y(y, height)), 0).rgb;
}

fn env_texel(direction: vec3<f32>) -> vec3<f32> {
    let dir = normalize(direction);
    let dimensions = textureDimensions(env_tex);
    let width = i32(dimensions.x);
    let height = i32(dimensions.y);
    let longitude = atan2(dir.z, dir.x) / TAU;
    let u = longitude - floor(longitude);
    let v = acos(clamp(dir.y, -1.0, 1.0)) / PI;
    // Nearest texel: the importance sampler draws a constant radiance per
    // texel, so the PDF used for MIS must describe that same distribution.
    let x = clamp(i32(floor(u * f32(width))), 0, width - 1);
    let y = clamp(i32(floor(v * f32(height))), 0, height - 1);
    return env_texel_at(x, y);
}

/// v2 bilinear radiance: wrap in X, clamp in Y, same formula as the CPU.
fn env_texel_bilinear(direction: vec3<f32>) -> vec3<f32> {
    let dir = normalize(direction);
    let dimensions = textureDimensions(env_tex);
    let width = f32(dimensions.x);
    let height = f32(dimensions.y);
    let longitude = atan2(dir.z, dir.x) / TAU;
    let u = longitude - floor(longitude);
    let v = acos(clamp(dir.y, -1.0, 1.0)) / PI;
    let fx = u * width - 0.5;
    let fy = v * height - 0.5;
    let x0 = i32(floor(fx));
    let y0 = i32(floor(fy));
    let tx = fx - f32(x0);
    let ty = fy - f32(y0);
    let top = mix(env_texel_at(x0, y0), env_texel_at(x0 + 1, y0), tx);
    let bottom = mix(env_texel_at(x0, y0 + 1), env_texel_at(x0 + 1, y0 + 1), tx);
    return mix(top, bottom, ty);
}

fn env_sample_value(direction: vec3<f32>, v2_env: bool) -> vec3<f32> {
    if (v2_env) { return env_texel_bilinear(direction); }
    return env_texel(direction);
}

fn luminance(value: vec3<f32>) -> f32 {
    return dot(value, vec3<f32>(0.2126, 0.7152, 0.0722));
}

fn balance(first: f32, second: f32) -> f32 {
    let a = max(first, 0.0);
    let b = max(second, 0.0);
    let aa = a * a;
    let bb = b * b;
    return aa / (aa + bb + 1.0e-20);
}

struct EnvEval {
    radiance: vec3<f32>,
    pdf: f32,
};

fn env_eval(direction: vec3<f32>) -> EnvEval {
    let total = params.env_params.z;
    if (total <= 0.0) {
        return EnvEval(vec3<f32>(0.0), 0.0);
    }
    let v2_env = (params.v2_flags.x & 4u) != 0u;
    let value = env_sample_value(direction, v2_env);
    let texel = env_texel(direction);
    let pdf = max(luminance(texel) / total, 1.0e-8);
    return EnvEval(value * params.env_params.w, pdf);
}

fn sample_sun() -> vec3<f32> {
    let light = params.sun_dir.xyz;
    if (params.sun_cos_max >= 0.999999) {
        return light;
    }
    let cos_theta = 1.0 - rand_f32() * (1.0 - params.sun_cos_max);
    let sin_theta = sqrt(max(0.0, 1.0 - cos_theta * cos_theta));
    let phi = rand_f32() * TAU;
    let local = vec3<f32>(sin_theta * cos(phi), sin_theta * sin(phi), cos_theta);
    return normalize(basis_from_normal(light) * local);
}

fn sample_sun_from(u: vec2<f32>) -> vec3<f32> {
    let light = params.sun_dir.xyz;
    if (params.sun_cos_max >= 0.999999) {
        return light;
    }
    return to_world(uniform_cone(u.x, u.y, params.sun_cos_max), light);
}

fn distance_window(distance: f32, range: f32) -> f32 {
    if (range <= 0.0) {
        return 1.0;
    }
    let t = clamp(distance / range, 0.0, 1.0);
    let falloff = 1.0 - t * t * t * t;
    return falloff * falloff;
}

struct EnvDirectionSample {
    direction: vec3<f32>,
    radiance: vec3<f32>,
    pdf: f32,
};

fn sample_env() -> EnvDirectionSample {
    let u = vec2<f32>(rand_f32(), rand_f32());
    let jitter = vec2<f32>(rand_f32(), rand_f32());
    return sample_env_from(u, jitter);
}

fn sample_env_from(u: vec2<f32>, jitter: vec2<f32>) -> EnvDirectionSample {
    let width = u32(params.env_params.x);
    let height = u32(params.env_params.y);
    let total = params.env_params.z;
    if (width == 0u || height == 0u || total <= 0.0) {
        return EnvDirectionSample(vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(0.0), 0.0);
    }

    let row_target = u.x * total;
    var low = 0u;
    var high = height - 1u;
    while (low < high) {
        let middle = (low + high) / 2u;
        if (env_rows[middle] < row_target) {
            low = middle + 1u;
        } else {
            high = middle;
        }
    }
    let row = low;
    var row_start = 0.0;
    if (row > 0u) {
        row_start = env_rows[row - 1u];
    }
    let row_total = max(env_rows[row] - row_start, 1.0e-12);
    let column_target = u.y * row_total;
    let base = row * width;
    low = 0u;
    high = width - 1u;
    while (low < high) {
        let middle = (low + high) / 2u;
        if (env_cols[base + middle] < column_target) {
            low = middle + 1u;
        } else {
            high = middle;
        }
    }
    let column = low;
    let texel = textureLoad(env_tex, vec2<i32>(i32(column), i32(row)), 0).rgb;
    let u_inside = (f32(column) + jitter.x) / f32(width);
    let v = (f32(row) + jitter.y) / f32(height);
    let theta = v * PI;
    let phi = u_inside * TAU;
    let sin_theta = sin(theta);
    let direction = normalize(vec3<f32>(
        sin_theta * cos(phi),
        cos(theta),
        sin_theta * sin(phi),
    ));
    let v2_env = (params.v2_flags.x & 4u) != 0u;
    let value = env_sample_value(direction, v2_env);
    let pdf = max(luminance(texel) / total, 1.0e-8);
    return EnvDirectionSample(direction, value * params.env_params.w, pdf);
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) global_id: vec3<u32>) {
    let width = params.size.x;
    let height = params.size.y;
    if (global_id.x >= width || global_id.y >= height) {
        return;
    }
    let index = global_id.y * width + global_id.x;
    let position4 = gbuffer_pos[index];
    if (position4.w < 0.5) {
        return;
    }

    let v2_ld = (params.v2_flags.x & 1u) != 0u;
    let v2_geo = (params.v2_flags.x & 2u) != 0u;
    let v2_var = (params.v2_flags.x & 8u) != 0u;
    let v2_sao = (params.v2_flags.x & 16u) != 0u;

    let normal4 = gbuffer_nrm[index];
    let base_normal = normalize(normal4.xyz);
    let base_geometric = select(base_normal, unpack_geo_normal(normal4.w), v2_geo);
    let batch = min(params.batch, params.samples - params.sample_offset);
    var sum = vec3<f32>(0.0);
    var lum_sq_sum = 0.0;

    for (var sample = 0u; sample < batch; sample += 1u) {
        if (v2_ld) {
            smp_init(params.seed, index, params.v2_flags.y + sample, params.v2_flags.z);
        } else {
            rng_state = rand_seed(index, params.sample_offset + sample);
        }
        var point = position4.xyz;
        var normal = base_normal;
        var offset_normal = base_geometric;
        var depth = 0u;
        var sample_radiance = vec3<f32>(0.0);

        // Ambient occlusion is a single visibility ray per sample. Soft AO
        // weights the hit by (t / max_t)² instead of returning zero.
        if (params.mode == 1u) {
            var direction: vec3<f32>;
            if (v2_ld) {
                let u = next_2d();
                direction = to_world(cosine_hemisphere(u.x, u.y), base_normal);
            } else {
                direction = cosine_sample(base_normal);
            }
            let eps = select(params.eps, ray_eps(params.v2_misc.x, point), v2_geo);
            let origin = point + offset_normal * eps;
            var visibility = 0.0;
            if (v2_sao && params.v2_misc.y > 0.0) {
                let hit = trace(origin, direction, params.v2_misc.y);
                if (hit.valid) {
                    let ratio = clamp(hit.t / params.v2_misc.y, 0.0, 1.0);
                    visibility = ratio * ratio;
                } else {
                    visibility = 1.0;
                }
            } else if (!occluded(origin, direction, FAR)) {
                visibility = 1.0;
            }
            sum += vec3<f32>(visibility);
            if (v2_var) {
                lum_sq_sum += visibility * visibility;
            }
            continue;
        }

        loop {
            var direction_pair = vec2<f32>(0.0, 0.0);
            var sun_pair = vec2<f32>(0.0, 0.0);
            var env_pair = vec2<f32>(0.0, 0.0);
            var area_pair = vec2<f32>(0.0, 0.0);
            if (v2_ld) {
                direction_pair = next_2d();
                sun_pair = next_2d();
                env_pair = next_2d();
                area_pair = next_2d();
            }

            let eps = select(params.eps, ray_eps(params.v2_misc.x, point), v2_geo);
            let origin = point + offset_normal * eps;

            if (params.sun_enabled != 0u) {
                var light: vec3<f32>;
                if (v2_ld) {
                    light = sample_sun_from(sun_pair);
                } else {
                    light = sample_sun();
                }
                let cosine = dot(normal, light);
                if (cosine > 0.0 && !occluded(origin, light, params.shadow_dist)) {
                    sample_radiance += params.sun_color.rgb * cosine;
                }
            }

            // Scene lights match the CPU tracer: next-event estimation with a
            // shadow ray per light, inverse-square falloff, a smooth range
            // window, spot-cone falloff and uniform rectangle sampling for
            // area lights.
            let light_count = params.light_info.x;
            for (var light_index = 0u; light_index < light_count; light_index += 1u) {
                let light = lights[light_index];
                let kind = u32(light.position.w + 0.5);
                var light_point = light.position.xyz;
                if (kind == 2u) {
                    let basis = basis_from_normal(light.direction.xyz);
                    var u = 0.0;
                    var v = 0.0;
                    if (v2_ld) {
                        let rotation = vec2<f32>(0.7548776662, 0.5698402910) * f32(light_index);
                        let rotated = fract(area_pair + rotation);
                        u = rotated.x * 2.0 - 1.0;
                        v = rotated.y * 2.0 - 1.0;
                    } else {
                        u = rand_f32() * 2.0 - 1.0;
                        v = rand_f32() * 2.0 - 1.0;
                    }
                    light_point += basis[0] * (u * light.size.x)
                        + basis[1] * (v * light.size.y);
                }
                let to_light = light_point - point;
                let distance_squared = dot(to_light, to_light);
                if (distance_squared <= eps * eps) {
                    continue;
                }
                let distance = sqrt(distance_squared);
                let to_light_dir = to_light / distance;
                let cosine = dot(normal, to_light_dir);
                if (cosine <= 0.0) {
                    continue;
                }
                // Direction from the light toward the shaded point.
                let cos_light = -dot(light.direction.xyz, to_light_dir);
                if (kind != 0u) {
                    let facing = select(cos_light, abs(cos_light), light.size.w > 0.5);
                    if (facing <= 0.0) {
                        continue;
                    }
                }
                var cone = 1.0;
                if (kind == 1u) {
                    let span = light.color.w - light.direction.w;
                    if (abs(span) <= 1.0e-6) {
                        if (cos_light < light.direction.w) {
                            continue;
                        }
                    } else {
                        cone = smoothstep(light.direction.w, light.color.w, cos_light);
                    }
                    if (cone <= 0.0) {
                        continue;
                    }
                }
                let window = distance_window(distance, light.range.x);
                if (window <= 0.0) {
                    continue;
                }
                let limit = max(distance - eps * 2.0, eps);
                if (occluded(origin, to_light_dir, limit)) {
                    continue;
                }
                // Lambertian emitter cosine: area lights only. Point and spot
                // lights are specified by radiant intensity and carry none.
                var emit = 1.0;
                if (kind == 2u && (params.v2_flags.x & 32u) != 0u) {
                    emit = select(cos_light, abs(cos_light), light.size.w > 0.5);
                }
                sample_radiance += light.color.rgb
                    * (cosine * window / distance_squared * light.size.z * cone * emit);
            }

            if (params.env_params.x > 0.0 && params.env_params.z > 0.0) {
                var environment: EnvDirectionSample;
                if (v2_ld) {
                    let jitter = vec2<f32>(fb(), fb());
                    environment = sample_env_from(env_pair, jitter);
                } else {
                    environment = sample_env();
                }
                let cosine = dot(normal, environment.direction);
                if (cosine > 0.0
                    && environment.pdf > 0.0
                    && !occluded(origin, environment.direction, params.shadow_dist))
                {
                    let pdf_cos = cosine / PI;
                    let weight = balance(environment.pdf, pdf_cos);
                    sample_radiance += environment.radiance * (cosine / PI)
                        / environment.pdf * weight;
                }
            }

            var direction: vec3<f32>;
            if (v2_ld) {
                direction = to_world(cosine_hemisphere(direction_pair.x, direction_pair.y), normal);
            } else {
                direction = cosine_sample(normal);
            }
            let hit = trace(origin, direction, FAR);
            if (hit.valid) {
                if (depth >= params.bounces) {
                    break;
                }
                point = hit.point;
                normal = hit.normal;
                if (v2_geo) {
                    let geometric = face_normal_toward(hit.face, -direction);
                    offset_normal = geometric;
                    if (dot(normal, geometric) < 0.0) {
                        normal = -normal;
                    }
                } else {
                    if (dot(normal, direction) > 0.0) {
                        normal = -normal;
                    }
                    offset_normal = select(-hit.face, hit.face, dot(hit.face, direction) < 0.0);
                }
                depth += 1u;
            } else {
                let environment = env_eval(direction);
                if (environment.pdf > 0.0) {
                    let cosine = max(dot(normal, direction), 0.0);
                    let pdf_cos = cosine / PI;
                    let weight = balance(pdf_cos, environment.pdf);
                    sample_radiance += environment.radiance * weight;
                }
                break;
            }
        }
        // Keep the raw HDR path radiance (no fixed 64 clamp, which darkened
        // bright HDR sources); the denoisers handle HDR input.
        let path = sample_radiance + params.fill.rgb;
        sum += path;
        if (v2_var) {
            let lum = luminance(path);
            lum_sq_sum += lum * lum;
        }
    }

    var variance_sum = 0.0;
    if (v2_var) {
        variance_sum = lum_sq_sum;
    }
    accum[index] = accum[index] + vec4<f32>(sum, variance_sum);
}
