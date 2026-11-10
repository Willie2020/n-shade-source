# Single-Image PBR Map Generation: Technical Specification

An add-on module for the Normal Studio (Rust + Bevy) project. From one input image it generates normal, height, displacement, AO, cavity, edge wear, roughness and metallic maps. Every map is generated only when its toggle is on. It reuses the math and conventions from the main spec (`ns_core`: height↔normal, RNM and derivative blending, curvature, AO, distance transforms).

## 1. Scope and honest quality expectations

**Outputs:** Normal, Height, Displacement, AO, Cavity, Edge Wear, Roughness, Metallic. Internal intermediates (delit base colour, curvature) can optionally be exported too.

**Two classes of map, with different reliability:**

| Class | Maps | Reliability |
| --- | --- | --- |
| Geometry-derived | Normal, Height, Displacement, AO, Cavity, Curvature, Edge Wear | High, deterministic, native resolution, once height/normal are good |
| Appearance estimates | Base colour (delit), Roughness, Metallic | Plausible, not measured. Single-image inverse rendering is ill-posed |

What the current research tells us:

- Ubisoft La Forge's CHORD (SIGGRAPH Asia 2025, open weights) is a diffusion-based model that predicts base colour, normal, height, roughness and metalness from a single image. It predicts base colour first, derives an irradiance-like image to predict normals, integrates the normals into height, then re-renders the maps to recover roughness and metalness.
- Its authors report it is tuned for 1024 px inputs, works best on organic materials, struggles with strongly specular surfaces such as metal, finds metalness hardest to predict, and is not yet production-ready according to artists who tested it.
- Other single-image material models exist (RGB↔X, SuperMat for albedo/roughness/metallic), but metallic and roughness remain the weakest outputs everywhere.

**Design consequence:** use ML for global, semantic estimates (base colour, low-frequency normals, roughness and metallic priors) and classical, resolution-independent processing for fine detail and every derived map. Always give the user controls (class presets, levels, paint masks) to correct roughness and metallic, rather than promising a one-click answer.

## 2. Design principles

1. **Everything off by default.** A map is generated only when its toggle is on, or when it is a required dependency of an enabled map.
2. **Stage graph, not a list of maps.** Shared intermediates (delit image, geometry, curvature) are computed once and reused.
3. **Content-addressed cache.** A result is reused when its inputs, settings, dependencies and backend are unchanged. Toggling off and on again is instant.
4. **Progressive quality.** Draft results appear fast and are replaced by High results when ready. Every job is cancellable.
5. **Hybrid ML + classical** with a pluggable backend, so the tool works with no ML installed (classical only) and improves when a model is available.
6. **Float pipeline.** All intermediates are f32 (CPU) or f16/f32 (GPU). Quantize only on export, with dithering.
7. **Non-destructive overrides.** Per-map levels, masks, painted corrections and constants sit on top of the generated result and survive regeneration.

## 3. Pipeline and dependency graph

```text
 input image
     │
   Prep ──► Delit ──► Geometry ──┬─► Displacement
                │      (height  ├─► Curvature ──┬─► Cavity
                │       normal) │               └─► EdgeWear ◄── Ao
                │               ├─► Ao ─────────────────────────┘
                └──► Roughness ◄┘ (also uses Geometry, Cavity, EdgeWear optionally)
                        │
                     Metallic (uses Delit, Roughness, optionally EdgeWear)
```

| Output map | Stage | Needs | Primary method | Cost |
| --- | --- | --- | --- | --- |
| Normal | Geometry | Delit | ML normal fused with classical detail (5.3) | High |
| Height | Geometry | Delit | Integrate fused normals, or classical luminance bands (5.2) | High |
| Displacement | Displacement | Geometry | Height remap to real units (5.4) | Low |
| AO | Ao | Geometry | Heightfield horizon AO (5.5) | Medium |
| Cavity | Cavity | Curvature | Concave part of multi-scale curvature (5.6) | Low |
| Edge wear | EdgeWear | Curvature, Ao | Convex edges broken up by tileable noise (5.7) | Low |
| Roughness | Roughness | Delit, Geometry | ML estimate or class-preset heuristic (5.8) | Medium |
| Metallic | Metallic | Delit, Roughness | ML, preset, or painted mask (5.9) | Medium |

Normal and Height are produced by the same Geometry stage, so enabling both costs the same as enabling one.

## 4. Input preparation (Prep and Delit stages)

**Prep**

- Decode to linear float. If the image is sRGB, convert with the exact sRGB EOTF. Keep a separate sRGB view only for ML models that expect it.
- Record the input hash (blake3 of the decoded pixels) for caching.
- **Working resolution:** Draft = long side 512, Standard = 1024, High = native. Models run at their trained size (tiled for larger images, section 6.3).
- Optional **denoise** (light bilateral or non-local means) before derivative operations, because derivatives amplify noise.
- Optional **make seamless** (Wrap mode): edge-blend or offset-and-blend before processing, so every stage can use wrap addressing.
- Optional **crop/tile selection** for large photos.

**Delit (flatten lighting and recover base colour)**

Classical path, valid for flat-lit texture photos:

```text
L        = luminance(linear image)
L_low    = gaussian(L, sigma_large)           // sigma_large ≈ 8–15% of the short side
delit    = image / max(L_low, eps) * mean(L_low)
delit    = soft_clamp_highlights(delit, p=99.5)   // suppress baked speculars
```

ML path: use the base colour output of the active backend (CHORD predicts it first). Either result is cached as `Delit` and consumed by later stages. Exposing `Delit` as an export is optional and off by default.

## 5. Map algorithms

### 5.1 Conventions

Tangent space, +Z out, OpenGL green internally (see main spec section 3). Height is single-channel float; its real-world scale is set by `height_scale_px` (the height range expressed in pixels), shared by AO, curvature and displacement so they stay consistent.

### 5.2 Height

1. **ML chain (preferred when a backend is enabled):** predict normals, then integrate with the Fourier least-squares method from main spec section 4.5 (mirror-pad when not seamless). This mirrors the CHORD chain and gives globally coherent height.
2. **Classical fallback (luminance bands):** valid only when darker generally means lower, such as stone, bark or plaster in flat light.

```text
bands = [ gaussian(Ld, s_k) - gaussian(Ld, s_{k+1}) for k in 0..K ]   // Ld = delit luminance
height = sum_k  w_k * bands[k]
height = normalize_percentile(height, 0.5, 99.5)        // → 0..1
```

Default sigmas (px at 1024): 1, 2, 4, 8, 16, 32 with weights emphasizing mid bands. Expose an **invert** toggle and per-band weights. 3. After either path, apply a **high-pass** (subtract a very large blur, scaled by a drift-removal slider) to kill low-frequency bowing from integration, then clamp. 4. Optionally run a **guided filter** (section 8.3) using the input luminance as the guide to keep height edges aligned with image edges at native resolution.

### 5.3 Normal

Two sources, fused in the **slope (derivative) domain**, which is linear and therefore safe to add and filter (main spec section 4.4):

- `n_ml`: ML-predicted normal (global shape, correct large-scale orientation).
- `n_cls`: classical normal from the final height via the Scharr operator (main spec section 4.1), at native resolution.

```rust
// low frequencies from the ML normal, high frequencies from the classical normal
let s_ml  = slope(&n_ml);                       // n.xy / n.z
let s_cls = slope(&n_cls);
let lo    = gaussian_vec2(&s_ml, sigma);        // sigma ≈ 4–8 px at 1024 (setting: detail_split)
let hi    = &s_cls - &gaussian_vec2(&s_cls, sigma);
let n     = normalize(vec3(lo + hi * detail_gain, 1.0));
```

If no ML backend is active, `n = n_cls`, built from the classical height with multi-scale strengths (large, medium, small bands blended with the derivative blend). Always renormalize and clamp `n.z ≥ 1e-4` before export.

### 5.4 Displacement

Displacement is not simply the height map renamed. It is height prepared for a renderer:

- **Midlevel** (default 0.5): the value that means zero displacement. Re-centre by subtracting the median (or a user-set level) and adding the midlevel.
- **Scale in real units:** `displacement_amount_m = (range_px * texel_size_m)`, using the document's texel density. Store the amount as export metadata and in the preview material.
- **Drift removal** (high-pass) stronger than for height, since tessellation shows low-frequency errors immediately.
- **Bit depth:** 16-bit PNG minimum, 32-bit float EXR recommended, with no banding from 8-bit quantization.
- **Tile safety:** in Wrap mode, make the first and last rows/columns identical in value, not just similar.

### 5.5 Ambient occlusion (heightfield horizon AO)

For each pixel, march in several directions and take the highest horizon angle within a radius:

```rust
pub fn heightfield_ao(h: &Grid<f32>, height_px: f32, radius_px: f32,
                      dirs: usize, steps: usize, strength: f32, wrap: Wrap) -> Grid<f32> {
    Grid::from_fn_par(h.w, h.h, |x, y| {
        let h0 = h.get(x, y) * height_px;
        let p0 = Vec2::new(x as f32, y as f32);
        let rot = interleaved_gradient_noise(x, y);          // per-pixel rotation, hides banding
        let mut occ = 0.0;
        for d in 0..dirs {
            let a = (d as f32 + rot) / dirs as f32 * std::f32::consts::TAU;
            let dir = Vec2::new(a.cos(), a.sin());
            let mut best = 0.0f32;
            for s in 1..=steps {
                let t = s as f32 / steps as f32;
                let r = radius_px * t * t;                   // denser samples near the pixel
                let dh = h.sample_bilinear(p0 + dir * r, wrap) * height_px - h0;
                let sin_e = dh / (dh * dh + r * r).sqrt();     // sine of the elevation angle
                best = best.max(sin_e * (1.0 - t * t));        // fade with distance
            }
            occ += best;
        }
        (1.0 - strength * occ / dirs as f32).clamp(0.0, 1.0)
    })
}
```

Run two radii (small, e.g. 4 px, and large, e.g. 32 px) and combine with a multiply or weighted blend for both fine crevices and broad shadowing. Defaults: 16 directions, 8 steps, then a 3×3 bilateral blur (depth-aware) to remove residual noise. High quality uses 32 directions. This has a direct WGSL port (one thread per pixel).

### 5.6 Curvature and cavity

Use a scale-normalized Laplacian of height at several scales. Positive means convex (edges), negative means concave (crevices):

```rust
pub fn curvature_ms(h: &Grid<f32>, sigmas: &[f32], weights: &[f32], wrap: Wrap) -> Grid<f32> {
    let mut acc = Grid::zeros(h.w, h.h);
    for (s, w) in sigmas.iter().zip(weights) {
        let g   = gaussian_blur(h, *s, wrap);
        let lap = laplacian(&g, wrap);          // 4-neighbour second differences
        acc.add_scaled(&lap, -*w * s * s);      // -Laplacian: convex positive; s² normalizes scales
    }
    acc
}
```

- Normalize with a robust percentile (for example divide by the 99th percentile of |curvature|), so one setting works across images.
- **Cavity** = `smoothstep(lo, hi, max(-curv, 0))`, as a mask where 1 = deepest crevice. Offer an invert switch for multiply-style use (white = open).
- **Curvature map** (0.5 = flat) is exported on request: `0.5 + k·curv`.
- Curvature from the *final fused normal* (via the divergence of its slope field) gives crisper edges than from height alone when the ML normal carries detail. Offer both as a `source` setting.

### 5.7 Edge wear

Edge wear is a mask of where paint, coating or surface has worn away. It combines convexity, exposure and breakup noise, so it does not look like a plain edge-detect:

```text
convex   = max(curvature_norm, 0)
edge     = smoothstep(lo, hi, convex)                     // lo≈0.15, hi≈0.6
noise    = tileable_fbm(uv, freq, octaves, seed)          // 0..1, periodic when Wrap
breakup  = smoothstep(0.5 - c, 0.5 + c, noise + bias)     // c = contrast
exposure = ao                                             // high AO value = exposed
wear     = clamp( edge * lerp(1, breakup, breakup_amount)
                       * lerp(1, exposure, ao_influence)
                       + height_bias * height, 0, 1)
wear     = grain(wear, scratch_noise, scratch_amount)     // optional directional scratches
```

Parameters: width (scale of convexity), intensity, breakup amount/frequency/contrast, AO influence, height bias, seed (deterministic output per seed). Cavity is not used here. Keep a separate **dirt** mask from cavity if the user wants grime in crevices. Edge wear can drive other maps downstream (roughness shift, metallic exposure), see 5.8 and 5.9.

### 5.8 Roughness

1. **ML estimate** from the backend when available. Then re-map with user levels. Treat this as a prior, since the CHORD authors themselves note it can be unreliable, and in one showcase discarded the roughness map in favour of a fixed value.
2. **Heuristic fallback (class presets):** pick a material class, which sets the range and base:

| Class | r\_lo | r\_hi | Base |
| --- | --- | --- | --- |
| Rough stone / concrete | 0.55 | 0.95 | 0.75 |
| Wood | 0.40 | 0.85 | 0.60 |
| Fabric / leather | 0.60 | 0.95 | 0.85 |
| Plastic / painted | 0.25 | 0.65 | 0.40 |
| Metal, polished | 0.10 | 0.45 | 0.25 |
| Metal, brushed / worn | 0.25 | 0.75 | 0.50 |
| Soil / mud / sand | 0.70 | 1.00 | 0.90 |

```text
hf     = normalize(gaussian(| Ld - gaussian(Ld, s) |, s2))      // local detail energy
r      = base + k_hf*(hf - 0.5) + k_cav*cavity + k_wear*edge_wear
r      = clamp(r, r_lo, r_hi)
```

`k_wear` is signed. Worn edges are smoother on bare polished metal (negative) but rougher on chipped paint (positive). Defaults are per class. 3. Always allow a **constant** mode and a **paint-over** mask. Many assets look best with a plain value plus authored variation.

### 5.9 Metallic

Metallic is close to binary in real materials, and it is the hardest map to predict from one image. Provide explicit modes, with **None** as the default:

| Mode | Behaviour |
| --- | --- |
| None | Constant 0. Safe default for non-metals |
| Constant | A single value, for example 1.0 for a whole metal sheet |
| ML | Backend estimate, then smoothed and optionally thresholded. Marked as low confidence in the UI |
| Auto-segment | Candidate metal where delit colour is low-saturation with high specular contrast, or matches a metal F0 hue table (gold, copper, brass, iron, aluminium). Threshold, morphological close/open, then edge-aware smoothing |
| From edge wear | `metallic = wear`, for the common chipped-paint-over-metal look (bare metal appears where paint wore away) |
| Painted | User brush strokes into a mask. Auto-segment output can seed it |

Output is a 0–1 mask, ideally near 0 or 1 with narrow transitions. Warn in the UI that it is an estimate.

## 6. ML backends

### 6.1 Backend trait

```rust
pub trait EstimatorBackend: Send + Sync {
    fn id(&self) -> &'static str;
    fn model_hash(&self) -> String;                       // part of the cache key
    fn capabilities(&self) -> BackendCaps;                // maps it can produce, native tile size, VRAM estimate
    fn estimate(
        &self,
        input: &Image32,
        want: &MapSet,
        tile: TileConfig,
        cancel: &AtomicBool,
        progress: &dyn Fn(f32),
    ) -> Result<EstimatedMaps, GenError>;
}
```

Implement three backends behind it:

| Backend | What it is | When to use |
| --- | --- | --- |
| `Classical` | Pure Rust, no model | Always available. Default |
| `Onnx` | ONNX Runtime through the `ort` crate, with GPU execution providers where available | Shipped in-app inference once a model is exported to ONNX |
| `Sidecar` | A local process (Python or ComfyUI with the CHORD nodes) driven over HTTP or stdin/stdout JSON | Fastest way to get state-of-the-art results while the Rust path matures. Also lets users swap models |

`candle` or `burn` are possible pure-Rust inference alternatives. Diffusion UNets are large, so ONNX Runtime or a sidecar is the pragmatic first choice.

### 6.2 Model management

- Do not bundle weights. Download on first enable of an ML-dependent map, with progress, SHA-256 verification and a cache folder under the user's data directory.
- **Check licences before shipping.** Verify the licence of the model weights and the datasets they were trained on (CHORD's open weights were trained on MatSynth). Do not assume commercial use is allowed.
- Show VRAM needs before loading, and unload the model when all ML-dependent maps are toggled off.

### 6.3 Tiled inference

Models trained at 1024 px should not be run on a 4K image in one pass or by naive resizing, which loses detail.

1. Split into tiles of the model's native size with 12–25% overlap.
2. Run each tile (wrap-pad edges in Wrap mode).
3. Blend with a raised-cosine window per tile and accumulate `sum(value·w) / sum(w)` in float.
4. For normals, blend slopes (not RGB), then renormalize.
5. Run the model also on a downscaled whole image for global context, then fuse by frequency (low from global, high from tiles) to avoid tile-to-tile tone shifts.

If you instead upscale 1024-px outputs, use the guided filter in 8.3 rather than bicubic, since cascading upscalers accumulates error.

## 7. Toggle and scheduling architecture

### 7.1 Data model

```rust
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum MapKind { Normal, Height, Displacement, Ao, Cavity, Curvature, EdgeWear, Roughness, Metallic, Delit }

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum Stage { Prep, Delit, Geometry, Displacement, Curvature, Ao, Cavity, EdgeWear, Roughness, Metallic }

impl MapKind {
    pub fn stage(self) -> Stage {
        use MapKind::*;
        match self {
            Normal | Height => Stage::Geometry,
            Displacement => Stage::Displacement,
            Ao => Stage::Ao, Cavity => Stage::Cavity, Curvature => Stage::Curvature,
            EdgeWear => Stage::EdgeWear, Roughness => Stage::Roughness,
            Metallic => Stage::Metallic, Delit => Stage::Delit,
        }
    }
}

impl Stage {
    pub fn deps(self) -> &'static [Stage] {
        use Stage::*;
        match self {
            Prep => &[],
            Delit => &[Prep],
            Geometry => &[Delit],
            Displacement | Curvature | Ao => &[Geometry],
            Cavity => &[Curvature],
            EdgeWear => &[Curvature, Ao],
            Roughness => &[Delit, Geometry],
            Metallic => &[Delit, Roughness],
        }
    }
}

pub struct MapSettings { pub enabled: bool, pub quality: Quality, pub params: MapParams }
pub enum Quality { Draft, Standard, High }

pub struct PbrJobConfig {
    pub maps: HashMap<MapKind, MapSettings>,
    pub backend: BackendChoice,         // Classical | Onnx(model) | Sidecar(url)
    pub auto_update: bool,              // regenerate on settings change vs. only on button press
}
```

### 7.2 Planning: enabling a map pulls in its dependencies

```rust
pub fn plan(enabled: impl Iterator<Item = MapKind>) -> Vec<Stage> {
    fn visit(s: Stage, seen: &mut HashSet<Stage>, out: &mut Vec<Stage>) {
        if !seen.insert(s) { return; }
        for d in s.deps() { visit(*d, seen, out); }
        out.push(s);                       // post-order = valid topological order
    }
    let (mut seen, mut out) = (HashSet::new(), Vec::new());
    for m in enabled { visit(m.stage(), &mut seen, &mut out); }
    out
}
```

Stages that exist only as dependencies are marked **hidden intermediate** in the UI (shown as a small link, for example "Roughness needs: Delit, Geometry"). They are cached but not exported unless their own toggle is on.

### 7.3 Cache keys and invalidation

```text
key(stage) = blake3( stage_id,
                     input_hash,
                     stage_settings_hash,          // only the params that stage reads
                     keys of all dependency stages,
                     backend_id + model_hash,
                     quality,
                     code_version )
```

Because dependency keys feed in, changing the Geometry settings automatically invalidates AO, Cavity, EdgeWear and so on, and nothing else. Keep an in-memory LRU of results and an optional on-disk cache (EXR or zstd f16) so reopening a project does not recompute.

### 7.4 Scheduling in Bevy

- Run jobs on `AsyncComputeTaskPool`. A resource holds the running tasks per stage with a **generation counter**. Results from an older generation are discarded, which prevents stale results from overwriting newer ones when the user keeps dragging sliders.
- Each task receives an `Arc<AtomicBool>` cancel flag, checked between tiles and between rows of long loops.
- Progress is reported through a channel or atomic float, surfaced in the UI as a per-map progress bar.
- Run at most one GPU-heavy stage at a time (model inference), but let cheap CPU stages overlap.
- Debounce slider changes by 150–250 ms, and use Draft quality while a slider is being dragged, then a full-quality pass on release.

```rust
#[derive(Resource, Default)]
pub struct GenJobs {
    running: HashMap<Stage, (u64, Task<Result<StageOutput, GenError>>, Arc<AtomicBool>)>,
}

fn poll_jobs(mut jobs: ResMut<GenJobs>, mut store: ResMut<MapStore>) {
    use bevy::tasks::{block_on, futures_lite::future};
    jobs.running.retain(|stage, (generation, task, _cancel)| {
        match block_on(future::poll_once(task)) {
            Some(result) => {
                if *generation == store.generation(*stage) {     // drop stale results
                    store.put(*stage, result);
                }
                false                                              // finished: remove from running
            }
            None => true,
        }
    });
}
```

Task-pool and futures re-exports differ slightly between Bevy versions. Check the docs for yours.

### 7.5 UI behaviour

Per-map row: toggle, quality dropdown, status chip, and a settings fold-out.

| Status | Meaning |
| --- | --- |
| Off | Not generated. Costs nothing |
| Queued / Running n% | In the scheduler, with progress and a Cancel button |
| Ready | Cached and current |
| Stale | Inputs changed and auto-update is off. Shows a "Regenerate" button |
| Error | Message plus Retry. A failed ML stage can fall back to the classical backend |

Additional UI:

- A global **Generate all enabled** button and an **Auto-update** switch.
- Estimated time and VRAM before running an ML stage, and a warning for large images.
- Channel selector in the canvas (view any map), plus split view of input vs map.
- Per-map **levels** (black/white/gamma), **invert**, **override mask**, and **reset to generated**.

## 8. High-quality techniques

### 8.1 Frequency-split fusion

ML models see the global structure but are limited in resolution. Classical operators see every pixel but have no semantics. Fusing in the frequency domain (as in 5.3) takes the best of both. Apply the same principle to height: low band from the integrated ML normal, high band from the classical luminance-derived bands, with a user `detail_gain` slider.

### 8.2 Float pipeline and dithering

Keep float throughout and quantize once at export. Apply ±0.5 LSB triangular dither before 8-bit export to avoid banding in smooth gradients (especially AO and roughness). Normals: dither the XY components, then renormalize.

### 8.3 Edge-aware upsampling (guided filter)

To bring a low-resolution ML result (for example 1024) up to native resolution, use a guided image filter (He et al.) with the native-resolution delit luminance as the guide. Outputs keep their smoothness but their edges follow the real edges in the image. Apply it per channel for roughness, metallic, height and slopes (not for raw RGB normals).

### 8.4 Consistency check (re-render)

As in CHORD's chain, render the generated maps with a simple PBR shader under the original light direction (a single point light with a Lambert + GGX term is enough) and compare to the input luminance. Report an error score in the UI and use it to auto-tune `height_scale_px`, normal strength and roughness base. Offer this as a "Validate" action, not a default.

### 8.5 Seamless handling

In Wrap mode, every kernel (blur, Scharr, Laplacian, horizon AO, FFT, tiled inference) must use periodic addressing. Test by tiling the output 3×3 and checking for seams at the borders.

### 8.6 Hole and artifact cleanup

Clamp specular highlights in the input before height estimation (they read as bumps). Offer an inpaint mask for watermarks and obstructions, and apply a median filter to remove salt-and-pepper outliers in derived maps.

## 9. Live preview in Bevy

Map each output to a `StandardMaterial` slot where one exists:

| Map | Preview slot |
| --- | --- |
| Normal | `normal_map_texture` (linear, OpenGL; `flip_normal_map_y` for DirectX) |
| Roughness + Metallic | `metallic_roughness_texture`, packed G = roughness, B = metallic (glTF layout), plus constant `perceptual_roughness` and `metallic` set to 1.0 so the textures drive the values |
| AO | `occlusion_texture` (red channel) |
| Height / Displacement | `depth_map` with `parallax_depth_scale` for parallax mapping. True tessellated displacement is not built in, so treat it as a later feature |
| Delit base colour | `base_color_texture` (sRGB) |
| Cavity, Edge wear | No direct slot. Composite them into the preview base colour and roughness (a "preview-only" bake), or write a small `MaterialExtension` |

Only textures for enabled maps are bound. A disabled map falls back to a neutral constant, so toggling is visible immediately. Update by mutating the `Image` assets, as in the main spec section 7.2.

## 10. Export and channel packing

| Target | Packing |
| --- | --- |
| Separate maps | One file per map, linear, 8/16-bit PNG, TGA, or EXR |
| glTF / Bevy | Metallic-roughness: G = roughness, B = metallic. Occlusion as a separate map, or in R of the same image |
| Unreal ORM | R = occlusion, G = roughness, B = metallic |
| Unity HDRP mask map | R = metallic, G = occlusion, B = detail mask (optional, for example edge wear), A = smoothness (1 − roughness) |
| Displacement | 16-bit PNG or 32-bit EXR, with midlevel and amount in a sidecar `.json` |

Naming templates (`{name}_{map}.{ext}`), a DirectX/OpenGL normal switch, and export of only the enabled maps. Add presets for Unreal, Unity, Godot and Blender.

## 11. Module layout

```text
crates/ns_core/src/pbr/
├─ mod.rs            MapKind, Stage, plan(), PbrJobConfig
├─ cache.rs          content-addressed keys, LRU + on-disk store
├─ scheduler.rs      jobs, cancellation, generations, progress
├─ prep.rs           decode, denoise, seamless, resize
├─ delit.rs          lighting flattening, highlight suppression
├─ geometry.rs       height + normal fusion (5.2, 5.3)
├─ displacement.rs   midlevel, scale, drift removal
├─ ao.rs             horizon AO (CPU reference)
├─ curvature.rs      multi-scale curvature, cavity
├─ edgewear.rs       wear mask, tileable noise
├─ roughness.rs      class presets + heuristic
├─ metallic.rs       modes (none/const/ml/auto/wear/paint)
├─ guided.rs         guided filter, fusion helpers
└─ backend/
   ├─ mod.rs         EstimatorBackend trait
   ├─ classical.rs
   ├─ onnx.rs        `ort`
   ├─ sidecar.rs     HTTP / stdio client
   └─ tiling.rs      tile split/blend
crates/ns_gpu/shaders/pbr/   ao.wgsl, curvature.wgsl, blur.wgsl, guided.wgsl, edgewear.wgsl
crates/ns_app/src/pbr_ui.rs  toggles, status chips, progress, preview wiring
```

**Additional crates:** `blake3` (cache keys), `ort` (ONNX Runtime), `reqwest` or `ureq` (sidecar and model download), `sha2`, `directories` (cache folder), `noise` or your own periodic noise, `rustfft`, `rayon`, `image`, `exr`.

## 12. Settings reference (defaults)

| Map | Key parameters | Defaults |
| --- | --- | --- |
| Delit | sigma\_large, highlight clamp | 10% of short side, p99.5 |
| Height | source (ML / luminance), band weights, invert, drift removal, height\_scale\_px | ML if available, balanced weights, off, 0.5, 8 px per 1.0 |
| Normal | strength, detail\_split sigma, detail\_gain, DX/GL | 1.0, 6 px, 1.0, OpenGL |
| Displacement | midlevel, amount (m), drift removal | 0.5, 0.01, 0.8 |
| AO | radii, directions, steps, strength | 4 px and 32 px, 16, 8, 1.0 |
| Cavity | scales, lo, hi, invert | 1, 2, 4 px; 0.1; 0.6; off |
| Edge wear | width, intensity, breakup amount/freq/contrast, AO influence, seed | 2 px, 1.0, 0.7, 6, 0.3, 0.5, 1 |
| Roughness | class, mode (ML/heuristic/constant), k\_hf, k\_cav, k\_wear | Rough stone, heuristic, 0.3, 0.2, class default |
| Metallic | mode | None |

All numeric defaults are starting points to tune against your test set. Pixel values are given at 1024 px and scale with image size.

## 13. Evaluation and testing

- **Synthetic ground truth:** render known PBR materials (for example from an open dataset such as MatSynth, after checking each material's licence) under a fixed light, run the pipeline on the render, and compare each output to the ground truth with PSNR, SSIM and LPIPS. Track per-map scores over time, and expect low scores on metallic.
- **Unit tests:** `plan()` returns a valid topological order and includes every dependency, with no duplicates. Cache keys change if and only if relevant inputs change. Toggling a map off and on again hits the cache.
- **Determinism:** same input, settings and seed produce bit-identical CPU outputs.
- **Seams:** 3×3 tiled outputs show no border discontinuity in Wrap mode.
- **Numerics:** normals have unit length, AO and masks stay within 0–1, and no NaNs or infinities after any stage (assert in debug builds).
- **GPU vs CPU:** each WGSL pass matches the CPU reference within tolerance on random heightfields.
- **Golden images:** a small set of textures (stone, wood, metal plate, fabric, soil) with reviewed outputs for visual regression.
- **Cancellation:** cancelling mid-tile leaves no partial result in the store.

## 14. Performance targets

Targets to tune on your own hardware (2048×2048, mid-range GPU with about 8 GB VRAM):

| Stage | Target |
| --- | --- |
| Prep + Delit (classical) | < 100 ms |
| Geometry, classical | < 300 ms |
| Geometry, ML (tiled) | seconds to tens of seconds. Show progress |
| Curvature + Cavity | < 150 ms |
| AO, GPU | < 100 ms. CPU 16 directions: 1–3 s on many cores |
| Edge wear | < 100 ms |
| Roughness, heuristic | < 150 ms |
| Draft preview of any map at 512 px | < 100 ms |

Memory: budget about 16–32 bytes per pixel per float map on the CPU. At 4096² a single `Rgba32F` image is 256 MB, so use f16 on the GPU and stream tiles for large inputs.

## 15. Milestones

1. **P1: Framework** (1–2 weeks): `MapKind`/`Stage`/`plan()`, cache, scheduler, toggle UI with status chips, and the Classical backend skeleton.
2. **P2: Geometry + derived maps, classical** (2–3 weeks): Prep, Delit, luminance height, normal, displacement, curvature, cavity, horizon AO, edge wear, previews and export packing.
3. **P3: Roughness and metallic modes** (1–2 weeks): class presets, heuristic, constant, paint-over, auto-segment, from-edge-wear.
4. **P4: Sidecar ML backend** (2 weeks): HTTP/stdio client to a local model server (for example ComfyUI with CHORD nodes), model download and tiling.
5. **P5: Fusion and quality** (2–3 weeks): frequency-split fusion, guided upsampling, seamless handling, dithering, validate action.
6. **P6: Native ONNX backend** (3–4 weeks): export, `ort` integration, GPU execution providers, VRAM management.
7. **P7: GPU ports** (2–3 weeks): AO, curvature, blur and guided filter in WGSL, validated against the CPU reference.
8. **P8: Polish:** presets per engine, batch processing, project integration with the layer stack (generated maps appear as layers you can paint over).

A useful first release is P1 to P3: every geometry-derived map works with no ML installed.

## 16. Risks and limitations

- **Ill-posed problem.** Lighting, geometry and material are entangled in one photo. State this plainly in the UI, and always let the user override.
- **Metallic and roughness** are estimates. Do not market them as measured.
- **Baked shadows and highlights** in the input corrupt height and AO. Delighting helps but cannot fully fix this.
- **Non-texture photos** (objects, scenes) are out of distribution for texture-oriented models. Warn the user, and offer a crop tool.
- **Resolution mismatch:** models trained at 1024 px degrade when forced to 4K. Use tiling and guided upsampling rather than naive resizing.
- **Licensing:** verify model-weight and dataset licences, and respect the licence of the user's input images. Do not bundle weights without permission.
- **Model churn:** the research area moves quickly. The backend trait lets you replace a model without touching the rest of the pipeline.

## 17. References

- Ubisoft La Forge, *CHORD: Chain of Rendering Decomposition for PBR Material Estimation from Generated Texture Images* (SIGGRAPH Asia 2025): project page, arXiv 2509.09952, code `ubisoft/ubisoft-laforge-chord`, ComfyUI nodes `ubisoft/ComfyUI-Chord`.
- Zeng et al., *RGB↔X: Image Decomposition and Synthesis Using Material- and Lighting-aware Diffusion Models* (2024).
- Hong et al., *SuperMat: Physically Consistent PBR Material Estimation at Interactive Rates* (ICCV 2025).
- Vecchio et al., *MatSynth* dataset (check per-material licences).
- He, Sun, Tang, *Guided Image Filtering*.
- Frankot & Chellappa, *A Method for Enforcing Integrability in Shape from Shading Algorithms*.
- Barré-Brisebois & Hill, *Blending in Detail* (derivative and RNM normal blending).
