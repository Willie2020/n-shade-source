// GPU evaluator for the layer stack.
//
// A polygon layer is a union of one or more closed contours. The shader finds
// the nearest contour boundary with a signed-distance loop, recovers its
// outward gradient, applies the selected analytic bevel profile, and
// composites the normal with derivative/RNM/replace blending. Layers that the
// shader cannot evaluate analytically arrive as CPU-precomputed RGBA8
// normal+height pixels and are sampled directly. Painted masks arrive as cached
// coverage bytes. Height is written to a second storage texture for the 2D
// height view.

struct Header {
    size_x: u32,
    size_y: u32,
    layer_count: u32,
    vertex_count: u32,
    height_scale: f32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
};

struct Layer {
    kind: u32,          // 0 = flat, 1 = polygon, 2 = fill, 3 = rounded rect, 4 = ellipse, 5 = texture
    blend: u32,         // 0 = derivative, 1 = RNM, 2 = replace, 3 = whiteout, 4 = UDN
    vertex_offset: u32, // polygon vertex offset; texture layers: pixel offset
    vertex_count: u32,
    flags: u32,         // bit 0 = invert, bit 2 = stroke, bits 3-5 = profile id
    mask_index: u32,    // 0xFFFFFFFF = no mask
    radius: f32,
    height: f32,
    opacity: f32,
    chamfer_deg: f32,
    corner_radius: f32,
    softness: f32,
    stroke_width: f32,
    edge_aa: f32,
    center: vec2<f32>,
    half_size: vec2<f32>,
    aabb_min: vec2<f32>,
    aabb_max: vec2<f32>,
};

struct Mask {
    kind: u32,
    vertex_offset: u32,
    vertex_count: u32,
    flags: u32,
    center: vec2<f32>,
    half_size: vec2<f32>,
    corner_radius: f32,
    opacity: f32,
    _pad: vec2<f32>,
};

struct Contour {
    vertex_offset: u32,
    vertex_count: u32,
    aabb_min: vec2<f32>,
    aabb_max: vec2<f32>,
};

@group(0) @binding(0) var<uniform> header: Header;
@group(0) @binding(1) var<storage, read> layers: array<Layer>;
@group(0) @binding(2) var<storage, read> vertices: array<vec2<f32>>;
@group(0) @binding(3) var normal_out: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(4) var height_out: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(5) var<storage, read> masks: array<Mask>;
@group(0) @binding(6) var<storage, read> contours: array<Contour>;
@group(0) @binding(7) var<storage, read> layer_pixels: array<u32>;
@group(0) @binding(8) var<storage, read> mask_pixels: array<u32>;

fn slope_of(n: vec3<f32>) -> vec2<f32> {
    return n.xy / max(n.z, 1e-4);
}

fn from_slope(s: vec2<f32>) -> vec3<f32> {
    return normalize(vec3<f32>(s, 1.0));
}

fn with_slope_opacity(n: vec3<f32>, opacity: f32) -> vec3<f32> {
    return from_slope(slope_of(n) * clamp(opacity, 0.0, 1.0));
}

fn rnm(base: vec3<f32>, detail: vec3<f32>) -> vec3<f32> {
    let t = base + vec3<f32>(0.0, 0.0, 1.0);
    let u = detail * vec3<f32>(-1.0, -1.0, 1.0);
    return normalize(t * dot(t, u) / max(t.z, 1e-4) - u);
}

fn whiteout(a: vec3<f32>, b: vec3<f32>) -> vec3<f32> {
    return normalize(vec3<f32>(a.xy + b.xy, a.z * b.z));
}

fn udn(a: vec3<f32>, b: vec3<f32>) -> vec3<f32> {
    return normalize(vec3<f32>(a.xy + b.xy, a.z));
}

fn blend_layer(acc: vec3<f32>, layer_n: vec3<f32>, mode: u32, opacity: f32) -> vec3<f32> {
    let o = clamp(opacity, 0.0, 1.0);
    if (o <= 0.0) {
        return acc;
    }
    if (mode == 2u) {
        // Replace fades between the surfaces in slope space; a vector lerp
        // would shorten both normals around the halfway point.
        return from_slope(mix(slope_of(acc), slope_of(layer_n), o));
    }
    let detail = with_slope_opacity(layer_n, o);
    if (mode == 0u) {
        return from_slope(slope_of(acc) + slope_of(detail));
    }
    if (mode == 3u) {
        return whiteout(acc, detail);
    }
    if (mode == 4u) {
        return udn(acc, detail);
    }
    return rnm(acc, detail);
}

// Profile ids match the GPU flags: 0 round, 1 chamfer, 2 linear, 3 convex,
// 4 concave, 5 smooth.
fn profile_height(t: f32, profile: u32, degrees: f32) -> f32 {
    let tc = clamp(t, 0.0, 1.0);
    if (profile == 1u) {
        return tc * max(tan(radians(degrees)), 0.0);
    }
    if (profile == 2u) {
        return tc;
    }
    if (profile == 3u) {
        return 1.0 - pow(1.0 - tc, 3.0);
    }
    if (profile == 4u) {
        return tc * tc * tc;
    }
    if (profile == 5u) {
        return tc * tc * (3.0 - 2.0 * tc);
    }
    return sqrt(max(1.0 - (1.0 - tc) * (1.0 - tc), 0.0));
}

// Returns (signed distance, outward gradient). Negative distance is inside.
fn polygon_sdf(p: vec2<f32>, offset: u32, count: u32) -> vec3<f32> {
    if (count < 3u) {
        return vec3<f32>(1e9, 0.0, 0.0);
    }

    var d2 = dot(p - vertices[offset], p - vertices[offset]);
    var closest = vertices[offset];
    var inside = false;
    var j = count - 1u;

    for (var i = 0u; i < count; i = i + 1u) {
        let vi = vertices[offset + i];
        let vj = vertices[offset + j];
        let e = vj - vi;
        let w = p - vi;
        let t = clamp(dot(w, e) / max(dot(e, e), 1e-8), 0.0, 1.0);
        let b = w - e * t;
        let dist2 = dot(b, b);
        if (dist2 < d2) {
            d2 = dist2;
            closest = vi + e * t;
        }

        let c1 = p.y >= vi.y;
        let c2 = p.y < vj.y;
        let c3 = e.x * w.y > e.y * w.x;
        if ((c1 && c2 && c3) || (!c1 && !c2 && !c3)) {
            inside = !inside;
        }
        j = i;
    }

    let dist = sqrt(d2);
    var grad = vec2<f32>(0.0, 0.0);
    let delta = p - closest;
    let delta_len = length(delta);
    if (delta_len > 1e-6) {
        grad = delta / delta_len;
        if (inside) {
            grad = -grad;
        }
    }

    return vec3<f32>(select(dist, -dist, inside), grad.x, grad.y);
}

fn sd_rounded_rect(p: vec2<f32>, center: vec2<f32>, half: vec2<f32>, r: f32) -> vec3<f32> {
    let half_size = max(half, vec2<f32>(1e-3, 1e-3));
    let radius = clamp(r, 0.0, min(half_size.x, half_size.y));
    let q = abs(p - center) - half_size + vec2<f32>(radius);
    let outside = max(q, vec2<f32>(0.0));
    let dist = length(outside) + min(max(q.x, q.y), 0.0) - radius;

    var grad = vec2<f32>(0.0, 0.0);
    let out_len = length(outside);
    if (out_len > 1e-6) {
        grad = normalize(sign(p - center) * outside);
    } else {
        // Inside the core. `q` is negative on both axes, so the outward
        // gradient points at the *nearest* edge: the axis with the larger q.
        // The old `sign(p - center)` fallback returned a 45 degree diagonal
        // for every interior pixel, which broke the bevel whenever the corner
        // radius was smaller than the bevel radius (worst at a pointy 0 px
        // corner, where the whole bevel band is inside the core).
        let sx = sign(p.x - center.x);
        let sy = sign(p.y - center.y);
        if (q.x > q.y) {
            grad = vec2<f32>(sx, 0.0);
        } else if (q.y > q.x) {
            grad = vec2<f32>(0.0, sy);
        } else {
            // The corner diagonal: both edges are equidistant.
            grad = normalize(vec2<f32>(sx, sy));
        }
        if (dot(grad, grad) < 1e-6) {
            grad = vec2<f32>(1.0, 0.0);
        }
    }
    return vec3<f32>(dist, grad.x, grad.y);
}

fn sd_ellipse(p: vec2<f32>, center: vec2<f32>, radii: vec2<f32>) -> vec3<f32> {
    let r = max(radii, vec2<f32>(1e-3, 1e-3));
    let q = (p - center) / r;
    let dist = (length(q) - 1.0) * min(r.x, r.y);
    var grad = vec2<f32>((p.x - center.x) / (r.x * r.x), (p.y - center.y) / (r.y * r.y));
    let grad_len = length(grad);
    if (grad_len > 1e-6) {
        grad = grad / grad_len;
    } else {
        grad = vec2<f32>(1.0, 0.0);
    }
    return vec3<f32>(dist, grad.x, grad.y);
}

// Union of disjoint filled contours. The signed distance of a union is the
// minimum of the member distances, and the gradient follows the nearest
// contour, which is exactly what the bevel needs.
fn box_distance(p: vec2<f32>, lo: vec2<f32>, hi: vec2<f32>) -> f32 {
    let outside = max(max(lo - p, p - hi), vec2<f32>(0.0));
    return length(outside);
}

fn compound_sdf(p: vec2<f32>, contour_offset: u32, contour_count: u32) -> vec3<f32> {
    if (contour_count == 0u) {
        return vec3<f32>(1e9, 0.0, 0.0);
    }
    var best = vec3<f32>(1e9, 0.0, 0.0);
    for (var i = 0u; i < contour_count; i = i + 1u) {
        let contour = contours[contour_offset + i];
        // Branch and bound: if the island's bounds are farther than the
        // nearest boundary found so far, no edge in it can win. Honeycomb and
        // slot banks contain hundreds of islands, so this skips most of them.
        if (i > 0u && box_distance(p, contour.aabb_min, contour.aabb_max) >= abs(best.x)) {
            continue;
        }
        let sdf = polygon_sdf(p, contour.vertex_offset, contour.vertex_count);
        if (i == 0u || sdf.x < best.x) {
            best = sdf;
        }
    }
    return best;
}

fn sdf_for_shape(
    p: vec2<f32>,
    kind: u32,
    vertex_offset: u32,
    vertex_count: u32,
    center: vec2<f32>,
    half_size: vec2<f32>,
    corner_radius: f32,
) -> vec3<f32> {
    if (kind == 3u) {
        return sd_rounded_rect(p, center, half_size, corner_radius);
    }
    if (kind == 4u) {
        return sd_ellipse(p, center, half_size);
    }
    return compound_sdf(p, vertex_offset, vertex_count);
}

fn shape_sdf(p: vec2<f32>, layer: Layer) -> vec3<f32> {
    return sdf_for_shape(
        p,
        layer.kind,
        layer.vertex_offset,
        layer.vertex_count,
        layer.center,
        layer.half_size,
        layer.corner_radius,
    );
}

fn soft_shape_sdf(p: vec2<f32>, layer: Layer) -> vec3<f32> {
    if (layer.softness <= 0.01) {
        return shape_sdf(p, layer);
    }
    var sum = vec3<f32>(0.0);
    var weight_sum = 0.0;
    for (var i = -2; i <= 2; i = i + 1) {
        for (var j = -2; j <= 2; j = j + 1) {
            let d = vec2<f32>(f32(i), f32(j));
            let w = exp(-0.5 * dot(d, d));
            sum = sum + shape_sdf(p + d * layer.softness, layer) * w;
            weight_sum = weight_sum + w;
        }
    }
    return sum / weight_sum;
}

fn mask_coverage(p: vec2<f32>, mask: Mask) -> f32 {
    // Painted / static mask: one coverage byte per document pixel.
    if (mask.kind == 5u) {
        let coord = vec2<u32>(p);
        let packed = mask_pixels[mask.vertex_offset + coord.y * header.size_x + coord.x];
        return f32(packed & 255u) / 255.0 * mask.opacity;
    }
    let sdf = sdf_for_shape(
        p,
        mask.kind,
        mask.vertex_offset,
        mask.vertex_count,
        mask.center,
        mask.half_size,
        mask.corner_radius,
    );
    let stroke = (mask.flags & 1u) != 0u;
    if (stroke) {
        let half = max(mask._pad.x, 0.5) * 0.5;
        let band = half - abs(sdf.x);
        return smoothstep(-0.5, 0.5, band) * mask.opacity;
    }
    return smoothstep(0.5, -0.5, sdf.x) * mask.opacity;
}

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= header.size_x || id.y >= header.size_y) {
        return;
    }

    let p = vec2<f32>(f32(id.x), f32(id.y));
    var acc = vec3<f32>(0.0, 0.0, 1.0);
    var height_acc = 0.0f;

    for (var i = 0u; i < header.layer_count; i = i + 1u) {
        let layer = layers[i];

        // Layers are expanded by their bevel radius and softness; outside that
        // box the layer contributes a flat normal. Skip the SDF work unless the
        // blend is Replace, which must flatten the accumulator globally.
        if (layer.kind == 1u || layer.kind == 3u || layer.kind == 4u) {
            if (layer.blend != 2u) {
                if (p.x < layer.aabb_min.x || p.y < layer.aabb_min.y ||
                    p.x > layer.aabb_max.x || p.y > layer.aabb_max.y) {
                    continue;
                }
            }
        }

        var layer_opacity = layer.opacity;
        if (layer.mask_index != 0xFFFFFFFFu) {
            layer_opacity = layer_opacity * mask_coverage(p, masks[layer.mask_index]);
        }

        if (layer.kind == 1u || layer.kind == 3u || layer.kind == 4u) {
            let sdf = soft_shape_sdf(p, layer);
            let invert = (layer.flags & 1u) != 0u;
            let profile = (layer.flags >> 3u) & 7u;
            let stroke = (layer.flags & 4u) != 0u;
            let radius = max(layer.radius, 1e-4);

            // The bevel band sits inside the shape for both modes; `invert`
            // carves the surface instead of moving the band outside (moving it
            // cancelled the normal flip and made inset and outset identical).
            // Outline strokes use the distance to the band centred on the
            // boundary instead.
            let fill_dist = -sdf.x;
            let stroke_dist = max(layer.stroke_width, 0.5) * 0.5 - abs(sdf.x);
            let dist = select(fill_dist, stroke_dist, stroke);
            let t = clamp(dist / radius, 0.0, 1.0);

            var layer_n = vec3<f32>(0.0, 0.0, 1.0);
            if (t > 0.0 && t < 1.0) {
                // z is the normal's height component; gain is dh/dt. Round and
                // chamfer follow the CPU analytic formula; linear, convex,
                // concave and smooth use their derivatives with z = 1.
                var z = 1.0;
                var fill_gain = 1.0;
                if (profile == 0u) {
                    z = sqrt(max(1.0 - (1.0 - t) * (1.0 - t), 0.0));
                    fill_gain = 1.0 - t;
                } else if (profile == 1u) {
                    let theta = radians(layer.chamfer_deg);
                    z = cos(theta);
                    fill_gain = max(sin(theta), 0.0);
                } else if (profile == 2u) {
                    fill_gain = 1.0;
                } else if (profile == 3u) {
                    fill_gain = 3.0 * (1.0 - t) * (1.0 - t);
                } else if (profile == 4u) {
                    fill_gain = 3.0 * t * t;
                } else {
                    fill_gain = 6.0 * t * (1.0 - t);
                }
                // The band distance peaks on the original boundary, where its
                // gradient is a cusp. Fade the lateral gain there to match the
                // CPU distance-field gradient instead of producing a seam.
                let stroke_gain = fill_gain * smoothstep(0.0, 1.0, abs(sdf.x));
                // A bevel's lateral slope is its rise over its run: height over
                // radius. Round and chamfer must scale with it or every shape
                // shades equally deep whatever Height says.
                let rise = clamp(layer.height / radius, -50.0, 50.0);
                var xy_gain = select(fill_gain, stroke_gain, stroke) * rise;
                // Edge anti-alias: fade the lateral slope to flat at the
                // silhouette so small shapes and small radii do not stair-step.
                let aa = layer.edge_aa;
                let edge_fade = select(1.0, smoothstep(0.0, max(aa, 1e-4), dist), aa > 1e-4);
                xy_gain = xy_gain * edge_fade;
                // `sdf.yz` is the outward gradient; `g` is the gradient of the
                // distance being beveled. The normal follows the CPU/OpenGL
                // convention n = (-dh/dx, +dh/dy) with rows increasing
                // downward, and `carve` flips the surface for inverted shapes.
                let fill_sign = -1.0;
                let stroke_sign = select(1.0, -1.0, sdf.x > 0.0);
                let g = sdf.yz * select(fill_sign, stroke_sign, stroke);
                let carve = select(1.0, -1.0, invert);
                layer_n = normalize(vec3<f32>(
                    vec2<f32>(-g.x, g.y) * xy_gain * carve,
                    z,
                ));
            }
            acc = blend_layer(acc, layer_n, layer.blend, layer_opacity);

            let h = profile_height(t, profile, layer.chamfer_deg);
            let shaped_h = select(h, 1.0 - h, invert);
            let normalized = shaped_h * layer.height / max(header.height_scale, 1e-4);
            height_acc = max(height_acc, normalized * clamp(layer_opacity, 0.0, 1.0));
        } else if (layer.kind == 5u) {
            // CPU-precomputed normal+height pixels (raster, smoothing, text,
            // text/photo/painted layers and shapes with holes). RGBA8: xyz is
            // the encoded normal, a is the normalized height.
            let index = layer.vertex_offset + id.y * header.size_x + id.x;
            let packed = layer_pixels[index];
            let r = f32(packed & 255u) / 255.0;
            let g = f32((packed >> 8u) & 255u) / 255.0;
            let b = f32((packed >> 16u) & 255u) / 255.0;
            let h = f32((packed >> 24u) & 255u) / 255.0;
            let decoded = vec3<f32>(r * 2.0 - 1.0, g * 2.0 - 1.0, b * 2.0 - 1.0);
            let layer_n = normalize(decoded + vec3<f32>(0.0, 0.0, 1e-6));
            acc = blend_layer(acc, layer_n, layer.blend, layer_opacity);
            height_acc = max(height_acc, h * clamp(layer_opacity, 0.0, 1.0));
        } else if (layer.kind == 2u) {
            acc = blend_layer(acc, vec3<f32>(0.0, 0.0, 1.0), layer.blend, layer_opacity);
            height_acc = max(height_acc, layer.height * clamp(layer_opacity, 0.0, 1.0));
        } else {
            acc = blend_layer(acc, vec3<f32>(0.0, 0.0, 1.0), layer.blend, layer_opacity);
        }
    }

    let coord = vec2<i32>(i32(id.x), i32(id.y));
    textureStore(normal_out, coord, vec4<f32>(acc * 0.5 + vec3<f32>(0.5), 1.0));
    let h = clamp(height_acc, 0.0, 1.0);
    textureStore(height_out, coord, vec4<f32>(h, h, h, 1.0));
}
