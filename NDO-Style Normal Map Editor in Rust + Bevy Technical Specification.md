# NDO-Style Normal Map Editor in Rust + Bevy: Technical Specification

A clean-room implementation guide covering the math, data model, shaders and Bevy integration. Verify Bevy API details against the release you use (Bevy changes its render and event APIs often). The math and WGSL here are version-independent.

## 1. Goals and scope

Build a standalone 2D normal map editor with a live 3D preview that reproduces the useful NDO workflows:

- Shape layers (raster masks or editable Bézier paths to bevelled normals) with editable bevel radius and profile, plus a pen/curve tool for drawing custom shapes.
- Photo/height to normal conversion.
- Height-space painting that converts to normals.
- Correct normal blending (RNM and derivative blending).
- Derived maps: height, AO, cavity, curvature.
- Non-destructive layer stack, export to PNG/TGA/EXR.

**Non-goals for v1:** texture painting (DDO), mesh baking, host-editor plugins. Use your own name and UI. Do not copy Quixel code, assets or branding.

## 2. Architecture

Separate the math from the engine. Bevy is the host (window, UI, preview, input), not the owner of the algorithms.

```text
normal-studio/
├─ Cargo.toml                 (workspace)
├─ crates/
│  ├─ ns_core/                pure Rust: math, layers, CPU reference, project format
│  │   ├─ src/convention.rs   tangent-space conventions, encode/decode
│  │   ├─ src/height.rs       height -> normal, normal -> height
│  │   ├─ src/path.rs         Bézier paths, primitives, booleans, rasterization (4.7)
│  │   ├─ src/sdf.rs          distance transform, bevel profiles
│  │   ├─ src/blend.rs        RNM, derivative blend
│  │   ├─ src/layers.rs       layer types, stack, evaluation
│  │   ├─ src/brush.rs        dab stamping
│  │   ├─ src/io.rs           image/exr import-export, project file
│  │   └─ src/derived.rs      AO, cavity, curvature
│  ├─ ns_gpu/                 WGSL shaders + wgpu/Bevy compute glue (phase 2)
│  └─ ns_app/                 Bevy app: UI, viewport, input
└─ assets/shaders/*.wgsl
```

**Key decision:** implement every operation first as a CPU reference in `ns_core` (using `rayon`), test it, then port hot paths to WGSL. The CPU version is your correctness oracle for GPU tests, and for 2K textures it is already fast enough for a usable v0.

**Suggested crates:** `bevy`, `bevy_egui` (UI), `glam`, `rayon`, `image`, `exr`, `rustfft`, `serde` + `ron`, `zstd`, `thiserror`, `proptest`.

**Path tool crates:** `lyon_path` / `lyon_geom` (Bézier flattening), `tiny-skia` (anti-aliased fill and stroke), `geo` or `i_overlay` (boolean ops), `usvg` (SVG import), optionally `kurbo` (curve math).

## 3. Conventions (decide these once, enforce everywhere)

| Item | Decision |
| --- | --- |
| Space | Tangent space, +Z out of the surface. |
| Image axes | x = column (right), y = row (down). |
| Green channel | Support both. Canonical internal form is **OpenGL (Y+)**. DirectX (Y-) is flipped on import/export only. |
| Internal storage | Signed unit vectors in `Rgba16Float` (GPU) or `Vec3` f32 (CPU). Never store 8-bit normals internally. |
| Export encoding | `rgb = n * 0.5 + 0.5`, 8- or 16-bit, **linear** (never sRGB). |
| Height | Single channel f32/f16, range 0–1 by convention, scaled by a document-level `height_scale` in pixel units. |
| Wrapping | Per-document: `Clamp` or `Wrap` (tileable textures need wrap everywhere). |

Bevy preview note: `StandardMaterial` expects OpenGL-style normal maps by default. Set `flip_normal_map_y: true` if you ever feed it a DirectX map. The normal texture must be loaded as linear, not sRGB.

## 4. Math specification

### 4.1 Height to normal

With `dh/dx` taken along increasing column and `dh/dy` along increasing row (image space, y down), and strength `s`:

- OpenGL (Y+): `n = normalize(-s·dh/dx, +s·dh/dy, 1)`
- DirectX (Y-): `n = normalize(-s·dh/dx, -s·dh/dy, 1)`

Use a Scharr kernel for derivatives (better rotational symmetry than Sobel):

```text
dh/dx = ( 3·(h[x+1,y-1] - h[x-1,y-1])
        + 10·(h[x+1,y]   - h[x-1,y])
        + 3·(h[x+1,y+1] - h[x-1,y+1]) ) / 32
dh/dy = same, transposed
```

The divisor of 32 gives the derivative in height-units per pixel. Offer an optional pre-blur (Gaussian) and multi-scale combination (large, medium, small radius with weights) for photo sources.

```rust
pub fn height_to_normal(h: &Grid<f32>, strength: f32, wrap: Wrap, flip_y: bool) -> Grid<Vec3> {
    let (w, hgt) = (h.w as i32, h.h as i32);
    Grid::from_fn_par(h.w, h.h, |x, y| {
        let s = |dx: i32, dy: i32| h.sample(x as i32 + dx, y as i32 + dy, wrap);
        let dx = (3.0 * (s(1,-1) - s(-1,-1)) + 10.0 * (s(1,0) - s(-1,0)) + 3.0 * (s(1,1) - s(-1,1))) / 32.0;
        let dy = (3.0 * (s(-1,1) - s(-1,-1)) + 10.0 * (s(0,1) - s(0,-1)) + 3.0 * (s(1,1) - s(1,-1))) / 32.0;
        let sy = if flip_y { -strength } else { strength };
        Vec3::new(-strength * dx, sy * dy, 1.0).normalize()
    })
}
```

### 4.2 Shape to bevel (the heart of an NDO-style tool)

The pipeline is: **mask → inside distance field → profile function → normal.** Because it is a function of the distance field, changing radius or profile never needs a repaint, which is what makes bevels editable.

1. **Mask**: rasterized selection, path fill, or brush layer (f32 0–1). Threshold at 0.5 for the distance transform.
2. **Inside distance `d(x,y)`**: distance (in pixels) from each inside pixel to the nearest outside pixel. Compute with an exact Euclidean distance transform (Felzenszwalb–Huttenlocher, section 4.3). Subtract 0.5 so the edge sits on the pixel boundary.
3. **Gradient of d**: `∇d` via central differences on `d` (after a tiny blur). Normalize it. Where `|∇d|` is \~0 (medial axis), the profile has already flattened out for typical radii.
4. **Profile**: let `t = clamp(d / R, 0, 1)` where `R` is the bevel radius in pixels.
   - **Round** (quarter circle): the normal is analytic, with no singularities: `n = ( ∇d·(t-1), sqrt(1-(1-t)²) )` in `(xy, z)`. For `t ≥ 1`, `n = (0, 0, 1)`.
   - **Chamfer** (45° or any angle θ): constant tilt, `n = (-sinθ·∇d, cosθ)` inside the band, flat beyond.
   - **Custom curve**: author a height profile `f(t)` as a 256-sample LUT. Slope `m = f'(t)·H/R` (H = bevel height in pixels). Then `n = normalize(-m·∇d, 1)`. Clamp `m` to about ±50 to avoid infinities at vertical walls.
5. **Outset vs inset**: invert `∇d` sign (and use the outside distance) for raised shapes.
6. **Softness**: blur the mask or `d` before step 3.

Because the gradient of `d` points inward, the surface normal tilts toward `-∇d` (outward), which is why `∇d` is multiplied by a negative-going factor.

### 4.3 Exact Euclidean distance transform (CPU)

Two 1D passes (columns, then rows) over squared distances.

```rust
const INF: f32 = 1e20;

fn edt_1d(f: &[f32], d: &mut [f32], v: &mut [usize], z: &mut [f32]) {
    let n = f.len();
    let mut k = 0usize;
    v[0] = 0;
    z[0] = f32::NEG_INFINITY;
    z[1] = f32::INFINITY;
    for q in 1..n {
        loop {
            let p = v[k];
            let s = ((f[q] + (q * q) as f32) - (f[p] + (p * p) as f32))
                / (2.0 * (q as f32 - p as f32));
            if s <= z[k] {
                k -= 1;
            } else {
                k += 1;
                v[k] = q;
                z[k] = s;
                z[k + 1] = f32::INFINITY;
                break;
            }
        }
    }
    k = 0;
    for q in 0..n {
        while z[k + 1] < q as f32 { k += 1; }
        let p = v[k];
        d[q] = (q as f32 - p as f32).powi(2) + f[p];
    }
}

/// mask: true = inside. Returns distance to nearest outside pixel.
pub fn inside_distance(mask: &Grid<bool>) -> Grid<f32> {
    // 1. f = 0 for outside pixels, INF for inside pixels
    // 2. run edt_1d down every column, then along every row (use rayon, one scratch buffer per thread)
    // 3. sqrt of the result, minus 0.5
    unimplemented!()
}
```

For wrap mode, tile the mask 3×3, run the transform, and crop the centre. For anti-aliased masks, refine the edge with the sub-pixel coverage value (`d -= (coverage - 0.5)` for edge pixels) to avoid stair-stepped bevels.

### 4.4 Normal blending

Never use linear blending or Overlay mode. Overlay destroys the blue channel, which is why NDO shipped a dedicated mix operation.

**Reoriented Normal Mapping (RNM)** (Barré-Brisebois & Hill) is the default for detail on a base. Inputs are unpacked vectors in \[-1, 1\]:

```text
t = base + (0, 0, 1)
u = detail * (-1, -1, 1)
r = normalize( t * dot(t, u) / t.z  -  u )
```

It handles the identity (flat detail returns base) and keeps detail following the base's slope.

**Derivative (height-domain) blend**: convert to slopes, add, convert back. This is exact for height-derived normals and is the right model for additive "stacking" of layers:

```text
slope(n) = n.xy / n.z
n_out    = normalize( vec3(slope(a) + slope(b), 1) )
```

**Opacity** scales the slope, not the colour: `slope' = slope · opacity`. This is physically meaningful (a half-opacity bevel is a half-height bevel).

Edge case: for a base normal pointing to -Z, RNM flips the detail by 180°. Clamp `n.z` to a small positive value when decoding imported maps.

```rust
pub fn rnm(base: Vec3, detail: Vec3) -> Vec3 {
    let t = base + Vec3::Z;
    let u = detail * Vec3::new(-1.0, -1.0, 1.0);
    (t * t.dot(u) / t.z - u).normalize()
}

pub fn derivative_blend(a: Vec3, b: Vec3) -> Vec3 {
    let s = a.truncate() / a.z.max(1e-4) + b.truncate() / b.z.max(1e-4);
    s.extend(1.0).normalize()
}
```

### 4.5 Normal to height

This is ill-posed (many height fields share nearly identical normals), so expose it as an approximation with modes. Treat normals as a gradient field and integrate:

1. Decode to slopes: `p = dh/dx = -nx/nz`, `q = dh/dy = ny/nz` (OpenGL convention above).
2. If not tileable, mirror-pad the image 2×2 to make it periodic.
3. 2D FFT both slope fields to get `P̂`, `Q̂` (`rustfft`, rows then columns).
4. Frankot–Chellappa least-squares integration. For frequencies `ωx`, `ωy` (`ω = 2π·k/N`), with `Ĥ(0,0) = 0`:

   `Ĥ = ( -i·ωx·P̂ - i·ωy·Q̂ ) / (ωx² + ωy²)`
5. Inverse FFT, crop the padding, and normalize to 0–1 by percentile clipping (e.g. 0.05th–99.95th) so outliers don't crush the contrast.

The low-frequency drift this produces can be damped by multiplying `Ĥ` with a high-pass factor `ω²/(ω²+ω0²)`. Provide two user modes: "all-round" (FFT, any normal map) and "hard-surface" (analytic, only for maps created by shape layers: just evaluate the stored shape layers' height directly instead of integrating).

### 4.6 Derived maps

- **Curvature**: compute the Laplacian of height (or the divergence of the normal's slope field). Positive is convex edges, negative is concave. Remap to 0.5 ± k·value.
- **Cavity**: smoothed negative curvature (and its multi-scale variant).
- **AO (height-based)**: for each pixel, sample N directions at several radii. AO is `1 - mean(max(0, atan((h[x+r·dir] - h[x]) / r)) / (π/2))`. Cheap and good enough for trimsheets.
- **Edge/convex mask**: threshold of curvature, useful as a wear mask.

### 4.7 Drawing curves and custom shapes (vector path tool)

Yes: shapes are drawn as editable Bézier paths, and the bevel engine turns any path into normals. Paths are stored as vector data in the layer, so you can keep editing nodes, handles, radius and profile at any time. This section supersedes the raw `mask` field in `ShapeLayer` (section 5): a shape layer now holds a `source: ShapeSource` and caches the rasterized mask from it.

**Data model** (`ns_core/src/path.rs`). Coordinates are f32 in document pixel space, y down, so paths survive a document resize.

```rust
pub enum ShapeSource {
    Raster(RasterRef),          // lasso / magic-wand selection, imported mask
    Path(Path),                 // pen-drawn Bézier shape
    Primitive(Primitive),       // rect (per-corner radii), ellipse, polygon, star, line
}

pub struct Path { pub subpaths: Vec<SubPath>, pub fill_rule: FillRule }   // NonZero | EvenOdd
pub struct SubPath { pub nodes: Vec<Node>, pub closed: bool }
pub struct Node {
    pub pos: Vec2,
    pub handle_in: Vec2,        // relative to pos; zero = no handle
    pub handle_out: Vec2,
    pub kind: NodeKind,         // Corner | Smooth | Symmetric
}
```

**Tools to build**

- **Pen**: click adds a corner node, click-drag adds a smooth node and pulls out symmetric handles. Enter or clicking the first node closes the path, Esc cancels.
- **Node edit**: select, move, delete, add a node on a segment, drag handles, convert corner/smooth, Alt-drag to break a handle, Shift to constrain to 45°, snap to grid and to other nodes.
- **Primitives**: rectangle with per-corner radius, ellipse, n-gon, star, rounded line. Each stays parametric until converted to a path.
- **Freehand**: sample pointer positions, simplify with Ramer–Douglas–Peucker, then fit cubic Béziers (Schneider's algorithm).
- **Boolean ops**: union, subtract, intersect and xor on closed shapes (flatten to polygons, then use a crate such as `geo` or `i_overlay`).
- **Offset/inset path**, **mirror/symmetry** across an axis, and later **SVG import** (`usvg`) and text outlines.
- **Open paths as strokes**: set a stroke width and the line becomes a closed outline, giving panel lines, grooves, ribs, cables and pipes.

**Rasterizing a path**

1. Flatten each cubic Bézier into line segments with an adaptive tolerance of about 0.1 px (`lyon_geom`/`lyon_path` do this).
2. Fill (or stroke) into an anti-aliased coverage mask, for example with `tiny-skia`, restricted to the path's bounding box expanded by `radius + 2` px.
3. Run the distance transform of section 4.3 on that box only, using the coverage value for the sub-pixel edge refinement, then the bevel step of section 4.2.

**Two generation modes**

| Mode | How it works | Use it for |
| --- | --- | --- |
| Raster SDF (default) | Path → mask → EDT → bevel profile | Any shape and topology, fastest to build, cheap to re-run on edit |
| Analytic path distance | Exact distance from each pixel to the flattened segments, sign from the winding rule | Smooth bevels at 4K+ or on tiny shapes, and variable bevel width along a path (interpolate the radius per node) |

The analytic mode costs O(pixels × segments), so add a uniform grid or BVH over the segments before using it at full resolution. Ship the raster mode first and add the analytic mode in a later milestone.

**Tube mode for open paths.** A round pipe or cable follows directly from the distance to the centreline, with no mask needed. For a pixel `p`, the closest centreline point `c` and tube radius `R`:

```text
o = (p - c) / R                       // offset in tube-radius units
if |o| < 1:  n = ( o.x, o.y, sqrt(1 - |o|²) )   // cylinder/hemisphere normal
else:        n = (0, 0, 1)
```

Blend overlapping tubes with the derivative blend of section 4.4 so crossings stay smooth.

**Curve editor for bevel profiles.** The custom profile in section 4.2 should reuse the same Bézier editing widget in 1D: the user drags handles on a unit square, and you sample the curve into the 256-entry LUT (keep x monotonic by clamping handle x values to the segment). One widget then serves both the shape paths and the profile.

**Bevy implementation**

- Draw the path overlay (curve, nodes, handles) with the egui `Painter` on the canvas. It is simpler than Bevy gizmos for 2D editor chrome.
- Hit-test nodes with a \~6 px screen-space radius and segments by distance to the flattened polyline.
- Run the pen as a small state machine:

```rust
enum PenState {
    Idle,
    Placing { sub: usize },                        // path under construction
    DraggingHandle { sub: usize, node: usize, out: bool },
    MovingNode { sub: usize, node: usize },
}
```

- Store path edits as undo commands holding before/after snapshots of the `Path` (paths are small, so snapshots are cheap).
- On any edit, mark the layer dirty so `evaluate_system` re-rasterizes only the bounding box and re-runs the EDT and bevel for that region.

**Pitfalls**

- Self-intersecting paths depend on the fill rule, so expose NonZero/EvenOdd per shape.
- Very sharp corners give pinched bevels. Offer an optional corner-rounding amount applied to the path before rasterizing.
- Shapes thinner than `2 × radius` never reach the full bevel height. Clamp the effective radius to half the local thickness, as noted in section 12.
- Keep path coordinates y-down everywhere to match the image convention in section 3.

## 5. Data model

```rust
#[derive(Serialize, Deserialize)]
pub struct Document {
    pub size: UVec2,
    pub wrap: Wrap,
    pub green: GreenChannel,          // OpenGL | DirectX (export convention)
    pub height_scale_px: f32,         // converts 0-1 height to pixel units
    pub layers: Vec<Layer>,           // bottom to top
}

pub struct Layer {
    pub id: Uuid,
    pub name: String,
    pub visible: bool,
    pub opacity: f32,                 // scales slope
    pub blend: BlendMode,             // Derivative | Rnm | Replace
    pub mask: Option<RasterRef>,      // optional layer mask (f32)
    pub kind: LayerKind,
}

pub enum LayerKind {
    Flat,                                         // (0,0,1)
    ImportedNormal { tex: RasterRef },
    PhotoHeight { tex: RasterRef, strength: f32, blur_radii: [f32; 3], weights: [f32; 3] },
    Shape(ShapeLayer),
    HeightPaint { tex: RasterRef, strength: f32 },
    Fill { height: f32 },
}

pub struct ShapeLayer {
    pub mask: RasterRef,              // cached coverage, regenerated from `source` on every edit
    pub source: ShapeSource,          // Raster | Path | Primitive (see 4.7)
    pub radius_px: f32,
    pub height_px: f32,
    pub profile: Profile,             // Round | Chamfer{deg} | Custom(Vec<f32> /*256 LUT*/)
    pub invert: bool,                 // outset vs inset
    pub softness_px: f32,
}
```

**Raster storage:** layers reference rasters (`RasterRef`) held in an asset store and saved as 16-bit PNG or `zstd`-compressed f16/f32 blobs. Keep parameters in the `.ron` project file so edits stay lossless.

**Evaluation:** walk the stack bottom to top into an accumulator `acc: Grid<Vec3>`, starting from flat.

```text
for layer in layers (visible only):
    n_l   = layer.eval()              // normal field for this layer
    n_l   = slope_scale(n_l, opacity * mask)
    acc   = blend(acc, n_l, layer.blend)
```

Cache `n_l` per layer plus a hash of its parameters. When a layer changes, re-evaluate it and everything above it. This incremental caching is what makes sliders feel real-time.

## 6. GPU compute (WGSL)

Use ping-pong textures: `acc_a` → `acc_b` per layer. Textures are `Rgba16Float` storage textures (or `Rgba32Float` if you see banding with very many layers). Workgroup size 8×8.

### 6.1 Height to normal

```wgsl
struct Params {
    size: vec2<u32>,
    strength: f32,
    flip_y: u32,
    wrap: u32,
    _pad: vec3<u32>,
};

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var height_tex: texture_2d<f32>;
@group(0) @binding(2) var out_tex: texture_storage_2d<rgba16float, write>;

fn load_h(c: vec2<i32>) -> f32 {
    let s = vec2<i32>(p.size);
    var q = c;
    if (p.wrap == 1u) { q = ((c % s) + s) % s; }
    else { q = clamp(c, vec2<i32>(0), s - vec2<i32>(1)); }
    return textureLoad(height_tex, q, 0).r;
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= p.size.x || id.y >= p.size.y) { return; }
    let c = vec2<i32>(id.xy);
    let dx = (3.0 * (load_h(c + vec2<i32>(1, -1)) - load_h(c + vec2<i32>(-1, -1)))
            + 10.0 * (load_h(c + vec2<i32>(1, 0)) - load_h(c + vec2<i32>(-1, 0)))
            + 3.0 * (load_h(c + vec2<i32>(1, 1)) - load_h(c + vec2<i32>(-1, 1)))) / 32.0;
    let dy = (3.0 * (load_h(c + vec2<i32>(-1, 1)) - load_h(c + vec2<i32>(-1, -1)))
            + 10.0 * (load_h(c + vec2<i32>(0, 1)) - load_h(c + vec2<i32>(0, -1)))
            + 3.0 * (load_h(c + vec2<i32>(1, 1)) - load_h(c + vec2<i32>(1, -1)))) / 32.0;
    let sy = select(p.strength, -p.strength, p.flip_y == 1u);
    let n = normalize(vec3<f32>(-p.strength * dx, sy * dy, 1.0));
    textureStore(out_tex, c, vec4<f32>(n, 1.0));
}
```

### 6.2 Shape bevel (from a precomputed distance texture)

Compute the distance field on the CPU (section 4.3) when the shape mask changes, and upload it as `R32Float`. Radius and profile edits then only re-run this shader.

```wgsl
struct BevelParams {
    size: vec2<u32>,
    radius: f32,
    height: f32,        // in pixels, used by custom profiles
    profile: u32,       // 0 round, 1 chamfer, 2 custom
    chamfer_angle: f32, // radians
    invert: u32,
    _pad: u32,
};

@group(0) @binding(0) var<uniform> bp: BevelParams;
@group(0) @binding(1) var dist_tex: texture_2d<f32>;
@group(0) @binding(2) var profile_lut: texture_1d<f32>;     // 256 samples of f(t)
@group(0) @binding(3) var out_tex: texture_storage_2d<rgba16float, write>;

fn d_at(c: vec2<i32>) -> f32 {
    let s = vec2<i32>(bp.size);
    return textureLoad(dist_tex, clamp(c, vec2<i32>(0), s - vec2<i32>(1)), 0).r;
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= bp.size.x || id.y >= bp.size.y) { return; }
    let c = vec2<i32>(id.xy);
    let d = d_at(c);
    let gx = (d_at(c + vec2<i32>(1, 0)) - d_at(c + vec2<i32>(-1, 0))) * 0.5;
    let gy = (d_at(c + vec2<i32>(0, 1)) - d_at(c + vec2<i32>(0, -1))) * 0.5;
    var g = vec2<f32>(gx, gy);
    let gl = length(g);
    if (gl > 1e-5) { g = g / gl; }
    let t = clamp(d / bp.radius, 0.0, 1.0);

    var n = vec3<f32>(0.0, 0.0, 1.0);
    if (d > 0.0 && t < 1.0) {
        if (bp.profile == 0u) {
            // round: n = (grad_d * (t - 1), sqrt(1 - (1-t)^2))
            n = vec3<f32>(g * (t - 1.0), sqrt(max(0.0, 1.0 - (1.0 - t) * (1.0 - t))));
        } else if (bp.profile == 1u) {
            n = vec3<f32>(-g * sin(bp.chamfer_angle), cos(bp.chamfer_angle));
        } else {
            let i = clamp(i32(t * 255.0), 0, 254);
            let f0 = textureLoad(profile_lut, i, 0).r;
            let f1 = textureLoad(profile_lut, i + 1, 0).r;
            let slope = clamp((f1 - f0) * 255.0 * bp.height / bp.radius, -50.0, 50.0);
            n = normalize(vec3<f32>(-g * slope, 1.0));
        }
        if (bp.invert == 1u) { n = vec3<f32>(-n.xy, n.z); }
    }
    textureStore(out_tex, c, vec4<f32>(n, 1.0));
}
```

### 6.3 Blend pass (RNM or derivative, with opacity and mask)

```wgsl
@group(0) @binding(0) var<uniform> bl: BlendParams;   // mode, opacity, size
@group(0) @binding(1) var base_tex: texture_2d<f32>;
@group(0) @binding(2) var layer_tex: texture_2d<f32>;
@group(0) @binding(3) var mask_tex: texture_2d<f32>;
@group(0) @binding(4) var out_tex: texture_storage_2d<rgba16float, write>;

fn slope(n: vec3<f32>) -> vec2<f32> { return n.xy / max(n.z, 1e-4); }

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= bl.size.x || id.y >= bl.size.y) { return; }
    let c = vec2<i32>(id.xy);
    let b = textureLoad(base_tex, c, 0).xyz;
    var d = textureLoad(layer_tex, c, 0).xyz;
    let w = bl.opacity * textureLoad(mask_tex, c, 0).r;
    d = normalize(vec3<f32>(slope(d) * w, 1.0));         // opacity scales slope

    var r: vec3<f32>;
    if (bl.mode == 0u) {                                  // derivative
        r = normalize(vec3<f32>(slope(b) + slope(d), 1.0));
    } else {                                              // RNM
        let t = b + vec3<f32>(0.0, 0.0, 1.0);
        let u = d * vec3<f32>(-1.0, -1.0, 1.0);
        r = normalize(t * dot(t, u) / t.z - u);
    }
    textureStore(out_tex, c, vec4<f32>(r, 1.0));
}
```

### 6.4 Other GPU passes

- **Preview encode**: `rgb = n * 0.5 + 0.5` into an `Rgba8Unorm`/`Rgba16Float` texture the `StandardMaterial` samples.
- **Gaussian blur**: separable, used for photo pre-blur and bevel softness.
- **Curvature, AO**: sampling shaders as in section 4.6.
- **Jump-flood distance transform** is possible on GPU, but the CPU EDT is simpler, exact, and only reruns when a mask changes. Start there.

## 7. Bevy integration

### 7.1 App structure

```rust
fn main() {
    App::new()
        .add_plugins((DefaultPlugins, bevy_egui::EguiPlugin::default(), NormalStudioPlugin))
        .run();
}

pub struct NormalStudioPlugin;

impl Plugin for NormalStudioPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DocumentRes>()      // wraps ns_core::Document
           .init_resource::<EvalCache>()        // per-layer cached normals + dirty flags
           .init_resource::<EditorState>()      // selected layer, active tool, brush settings
           .add_systems(Startup, setup_preview_scene)
           .add_systems(Update, (
               ui_system,            // egui: layer panel, sliders, curve editor
               canvas_input_system,  // pointer -> texture-space dabs / path points
               evaluate_system,      // re-evaluate dirty layers
               upload_system,        // push result into the preview Image
           ).chain());
    }
}
```

**Events:** use a buffered event (named `Message` in Bevy 0.17+) such as `DocumentChanged { layer: LayerId }` so `evaluate_system` knows where to restart evaluation.

### 7.2 Preview viewport

- Spawn a mesh (plane, sphere, rounded cube) with `StandardMaterial` and a directional/point light the user can orbit.
- Meshes need tangents. Use `Mesh::generate_tangents()` or primitives that supply `ATTRIBUTE_TANGENT`, otherwise Bevy's normal mapping will look wrong.
- Set the material's `normal_map_texture` to the handle of the Image you update. Mutating the `Image` asset each time evaluation finishes is enough to refresh the preview.
- For a tiling preview, set `uv_transform` on the material (e.g. scale 3×3) and use `ImageSampler::Descriptor` with `Repeat` address mode.

```rust
let normal_img = images.add(Image::new_fill(
    Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    TextureDimension::D2,
    &[128, 128, 255, 255],
    TextureFormat::Rgba8Unorm,     // linear, NOT Rgba8UnormSrgb
    RenderAssetUsages::default(),
));

commands.spawn((
    Mesh3d(meshes.add(Sphere::new(1.0).mesh().build().with_generated_tangents().unwrap())),
    MeshMaterial3d(materials.add(StandardMaterial {
        base_color: Color::srgb(0.7, 0.7, 0.7),
        normal_map_texture: Some(normal_img.clone()),
        flip_normal_map_y: false,        // true only if feeding DirectX-style normals
        perceptual_roughness: 0.5,
        ..default()
    })),
));
```

Exact constructor names (`Mesh3d`, `MeshMaterial3d`, `with_generated_tangents`) and the `Image::data` field type (`Option<Vec<u8>>` in recent versions) have shifted between Bevy releases. Check the docs for yours.

### 7.3 Two ways to run the compute pipeline

**Option A (recommended first): CPU evaluation.** `evaluate_system` runs `ns_core::evaluate_dirty(&doc, &mut cache)` on a background task (`AsyncComputeTaskPool`) so the UI never stalls. Write the result into the preview `Image` bytes. At 2048² with rayon this is a few tens of ms per pass, fine for interactive editing with a coarse preview during slider drags and a full-res refresh on release.

**Option B: GPU compute in Bevy's render world.** Follow Bevy's own compute-shader example pattern:

1. In `RenderApp`, create a `CachedComputePipelineId` per shader via `PipelineCache`.
2. Extract your document resource into the render world (`ExtractResource`).
3. Add a render graph node (or a system in the render schedule) that dispatches the passes in order, ping-ponging textures.
4. Make the final output texture the one the `StandardMaterial` samples, or copy it into the `GpuImage` that backs the material's normal map.

This API surface changes often. Build against a pinned Bevy version and keep the shader code in `ns_gpu` independent of Bevy so it can move to plain `wgpu` if needed.

### 7.4 UI (egui)

- **Left**: layer list (drag to reorder, eye icon, opacity slider, blend mode dropdown).
- **Right**: properties for the selected layer (bevel radius, height, profile curve editor via `egui_plot` or a custom painter, photo strength and blur; node, handle and stroke-width settings when a path layer is selected).
- **Centre**: 2D canvas showing the normal (or height, AO) with pan/zoom, plus a toggle for a split 3D view and a left-edge toolbar (select, pen, node edit, shapes, freehand, brush).
- Keep the egui panel state in `EditorState`, and send `DocumentChanged` events only on actual edits so dragging a slider doesn't re-evaluate unchanged layers.

### 7.5 Canvas input

Convert the cursor position to texture space:

```text
uv = (cursor - canvas_rect.min) / canvas_rect.size / zoom + pan
px = uv * doc.size
```

Feed `px` plus pressure (from tablet if available through winit's touch force) into the active tool. Ignore events while egui wants the pointer (`ctx.wants_pointer_input()`).

Route `px` to the active tool: pen, node-edit and shape tools edit the selected layer's `Path` (section 4.7), and brush tools stamp dabs (section 8). Paths live in document pixel space, so convert from canvas space once per event and never store screen coordinates.

## 8. Painting and sculpting

Paint into a **height raster** (the HeightPaint layer), not directly into normals. This keeps the stack non-destructive and makes undo trivial.

**Dab stamping** (per pointer sample, spaced along the stroke at \~25% of the brush diameter):

```rust
fn stamp(height: &mut Grid<f32>, centre: Vec2, radius: f32, hardness: f32, flow: f32, mode: BrushMode) {
    for (x, y) in bbox_pixels(centre, radius, height.size()) {
        let r = (Vec2::new(x as f32, y as f32) - centre).length() / radius;
        if r >= 1.0 { continue; }
        let falloff = smoothstep(1.0, hardness, r);        // 1 at core, 0 at edge
        let a = falloff * flow;
        let h = &mut height[(x, y)];
        *h = match mode {
            BrushMode::Raise => (*h + a).min(1.0),
            BrushMode::Lower => (*h - a).max(0.0),
            BrushMode::Smooth => lerp(*h, blurred_sample(x, y), a),
            BrushMode::Flatten(target) => lerp(*h, target, a),
        };
    }
}
```

- Track a **dirty rectangle** per stroke and re-evaluate only that region (plus a 2-pixel margin for the Scharr kernel).
- **Undo**: store the pre-stroke contents of the dirty rect (tile-based, compressed) per stroke.
- Extra tools later: smudge, lattice/liquify warp, clone, symmetry (mirror dabs across an axis), stamp patterns (alpha textures).

## 9. Import, export and project files

- **Import**: 8/16-bit PNG, TGA (`image` crate), EXR (`exr`). Detect or ask for the green convention. Optionally import SVG paths (\`usvg\`) as editable shape layers. Renormalize decoded normals and clamp `nz` to ≥ 1e-4.
- **Export**: normal (OpenGL or DirectX), height, AO, cavity, curvature. 16-bit PNG and EXR recommended, TGA 8-bit for older pipelines. Always write linear data.
- **Project file**: `project.nsproj` (zip) containing `doc.ron` (parameters) and `rasters/*.bin.zst`. Include a `version` field and write migration functions from day one.

## 10. Testing strategy

| Test | Check |
| --- | --- |
| Unit length | After every op, `abs(length(n) - 1) < 1e-3`. |
| RNM identity | `rnm(base, flat) == base` and `rnm(flat, detail) == detail` within tolerance. |
| Round trip | Height → normal → height (FFT) has low RMS error on smooth random fields, with a threshold you tune. |
| Bevel symmetry | A square shape produces symmetric bevels with sharp, matching corners (golden image). |
| Tiling | With `Wrap::Wrap`, output edges match opposite edges exactly. |
| GPU vs CPU | Every WGSL pass compared against the CPU reference on random inputs (max abs error < 1e-3 for f16 paths). |
| Property tests | `proptest` for blend commutativity cases and slope-scaling monotonicity. |

Keep a small `testdata/` set of golden PNGs and compare with a perceptual tolerance in CI.

**Path tests:** flattening stays within 0.1 px of the true Bézier; a closed square path rasterizes to the same mask as the equivalent raster square; boolean union/subtract match hand-built golden masks; node moves never change the winding of an untouched subpath; and a path edit followed by undo restores the exact normal map (bit-identical on the CPU path).

## 11. Milestones

1. **M1: ns\_core math** (2–3 weeks): grids, conventions, height→normal, EDT, round/chamfer bevels, RNM and derivative blend, tests.
2. **M2: Layer stack + CPU eval** (2–3 weeks): data model, caching, project save/load, export.
3. **M3: Bevy shell** (2 weeks): egui panels, 2D canvas, 3D preview with live normal map.
4. **M4: Shapes and photo layers** (2–3 weeks): pen/Bézier path tool with node editing, primitives, boolean ops and path rasterization (section 4.7, adds about 1–2 weeks), editable bevel UI, custom profile curve, photo-to-normal with multi-scale blur.
5. **M5: Painting** (2–3 weeks): brush engine, dirty-rect eval, undo.
6. **M6: Derived maps and normal→height** (2 weeks): FFT integration, curvature, AO, cavity.
7. **M7: GPU port** (3–4 weeks): WGSL passes validated against the CPU reference.
8. **M8: Polish**: presets, hotkeys, tablet support, docs, installers.

A usable MVP is M1 to M4.

## 12. Pitfalls

- **sRGB mistakes** are the most common bug. Normal maps are linear data. If the preview looks too flat or weirdly tinted, check texture formats first.
- **Green channel flip**: if lighting looks inverted vertically, the convention is wrong. Put a lit-from-top test image in your test suite.
- **Precision**: 8-bit intermediate normals will band after a few blends. Keep f16/f32 internally.
- **Tiling**: handle wrap in every kernel (blur, EDT, FFT padding), not just the first one.
- **Medial-axis artifacts** in bevels when radius exceeds half the shape's thickness. Clamp the effective radius to the local maximum of `d` or fade the profile.
- **Normal to height is approximate.** Label it clearly in the UI and offer the exact analytic path for shape-based layers.
- **Bevy API churn**: pin the version, isolate Bevy-specific code in `ns_app`, and keep `ns_core` and the shaders free of Bevy types.
- **Legal**: implement from public math (RNM, SDFs, Poisson integration), choose your own name and UI, and don't use Quixel's assets or code.

## 13. References

- Barré-Brisebois & Hill, *Blending in Detail* (Reoriented Normal Mapping), blog.selfshadow.com.
- Felzenszwalb & Huttenlocher, *Distance Transforms of Sampled Functions*.
- Frankot & Chellappa, *A Method for Enforcing Integrability in Shape from Shading Algorithms*.
- Bevy examples: compute shader, custom material, and `bevy_egui` integration.
