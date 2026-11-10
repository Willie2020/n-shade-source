# Normal Forge: Partial Cut, Partial Slash and Clip Divide

Technical specification for implementing vector hard-surface normal map operations in **Rust + Bevy**.
It is written for an implementer (human or AI) who also has the existing web prototype (`normal-forge.html`) as the behavioural reference.

---

## 0. Read this first

| Operation | Status in the web prototype | Status in this spec |
|---|---|---|
| Partial Cut (called **Pocket (partial cut)** in the UI) | Implemented, `j = 4` | Port exactly. Section 6.1 |
| Partial Slash | Not implemented | New, `j = 5`. Section 6.2 |
| Clip Divide | Not implemented | New, `j = 6`. Section 6.3 |

**Interpretation notice.** "Partial slash" and "clip divide" were not defined precisely in the request, so this spec defines them as follows. If the intent differs, change Sections 6.2 and 6.3 only; everything else stays valid.

* **Partial Slash**: a blade-like groove along a straight line. It has adjustable depth, a cross-section profile, rounded ends, and optional **taper** so the depth fades in and out along its length. It never cuts deeper than the layer beneath it, so it is "partial" in depth and, with taper, in length.
* **Clip Divide**: a straight seam that divides a layer into two separately bevelled pieces. It can be infinite (splits the whole layer) or finite (a partial divide that stops short). An infinite divide can also **step** one half up or down, like two sliding armour plates.

---

## 1. Compatibility contract (do not break existing behaviour)

These rules are mandatory. Treat any violation as a bug.

1. **Additive only.** Do not change the math, numbering or defaults of any existing mode, join, profile or shape kind. New behaviour is reached only through new enum values.
2. **Enum values are a file format.** Keep these numeric values forever:

   | Enum | Values |
   |---|---|
   | `ShapeKind` | Rect 0, Ellipse 1, Polygon 2 |
   | `Mode` | Raise 0, Cut 1, Groove 2, SetLevel 3, UnionMax 4, CarveMin 5 |
   | `Profile` | Chamfer 0, Round 1, Smooth 2, HardStep 3 |
   | `Join` | Layer 0, Union 1, Subtract 2, Intersect 3, **Pocket 4**, **Slash 5**, **ClipDivide 6** |

3. **Old projects must load unchanged.** New serialized fields use `#[serde(default)]` and default to `0.0`. A project with no `p1`/`p2` fields must render identically to before.
4. **One existing line must change, carefully.** The prototype detects a pocket with `d.x > 3.5`. That test would also catch values 5 and 6. In the port, compare with the integer join value (`j == 4`), never with `> 3.5`.
5. **Regression gate.** Before touching any shader or evaluator code, add the golden tests from Section 11 and make them pass. They must still pass, unchanged, after the new operations land.
6. **CPU reference is the source of truth.** The Rust CPU evaluator and the WGSL shader must agree within the tolerance in Section 11. If they disagree, fix the shader.

---

## 2. Coordinate systems and conventions

* **Reference space**: 1024 × 1024 units, origin top-left, **+Y points down**. All shape fields and all heights are in these units. Output resolution only changes the sampling density; a 20-unit bevel is 20 units at any export size.
* **Local frame of a shape**: translate by `(x, y)`, then rotate by `-r`.

  ```
  d   = p - (x, y)
  q.x =  cos(r) * d.x + sin(r) * d.y
  q.y = -sin(r) * d.x + cos(r) * d.y        // r in radians
  ```
* **Height** is in reference units. `0` is the base plane. Positive is raised, negative is recessed.
* **Normal** (OpenGL convention, +Y up in the image):

  ```
  gx = ∂h/∂x,   gy = ∂h/∂y   (y measured downward in the image)
  n  = normalize(-gx * strength, +gy * strength, 1)
  ```
  DirectX output negates `n.y`. Encode as `n * 0.5 + 0.5`.
* **Derivatives** use central differences with `e = 0.5 * 1024 / res` reference units.
* **Anti-aliasing**: `aa × aa` sub-pixel samples on a regular grid, average the normalized normals, then normalize again.
* **Wgpu/Bevy framebuffer origin is top-left**, unlike GLSL `gl_FragCoord`, which is bottom-left. In WGSL use `p = frag.xy * 1024 / res` with **no Y flip**. The prototype flips because of GLSL.

---

## 3. Data model

### 3.1 Fields

| JSON key | Rust field | Type | Meaning |
|---|---|---|---|
| `t` | `kind` | `ShapeKind` | Rect, Ellipse, Polygon |
| `m` | `mode` | `Mode` | Height mode of a layer |
| `x`, `y` | `x`, `y` | f32 | Centre |
| `w`, `h` | `w`, `h` | f32 | Full width and height |
| `r` | `rot_deg` | f32 | Rotation in degrees |
| `rad` | `corner` | f32 | Rect corner radius |
| `bev` | `bevel` | f32 | Bevel or wall width |
| `H` | `height` | f32 | Height, depth, or target level |
| `pf` | `profile` | `Profile` | Wall profile |
| `n` | `sides` | f32 | Polygon sides (3 to 12) |
| `j` | `join` | `Join` | How this shape combines with the previous one |
| `sm` | `blend` | f32 | Smooth blend radius for Union, Subtract, Intersect, ClipDivide |
| `p1` | `p1` | f32 | **New.** Operation parameter 1 (default 0) |
| `p2` | `p2` | f32 | **New.** Operation parameter 2 (default 0) |

The project file is `{ "shapes": [ ... ] }`. Unknown keys must be ignored so projects written by the web prototype and by future versions both load.

### 3.2 Meaning of `p1` and `p2` per join

| Join | `p1` | `p2` |
|---|---|---|
| 0 to 4 | unused | unused |
| Slash (5) | taper-in fraction, 0 to 0.5 | taper-out fraction, 0 to 0.5 |
| ClipDivide (6) | step in reference units, signed | `1` = infinite divide, `0` = finite |

### 3.3 Rust types

```rust
use glam::Vec2;
use serde::{Deserialize, Serialize};

pub const MAX_SHAPES: usize = 32;

macro_rules! u8_enum {
    ($name:ident { $($v:ident = $n:literal),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(try_from = "u8", into = "u8")]
        pub enum $name { $($v = $n),+ }
        impl TryFrom<u8> for $name {
            type Error = String;
            fn try_from(v: u8) -> Result<Self, String> {
                match v { $($n => Ok(Self::$v),)+ _ => Err(format!(concat!(stringify!($name), ": unknown value {}"), v)) }
            }
        }
        impl From<$name> for u8 { fn from(v: $name) -> u8 { v as u8 } }
    };
}

u8_enum!(ShapeKind { Rect = 0, Ellipse = 1, Polygon = 2 });
u8_enum!(Mode { Raise = 0, Cut = 1, Groove = 2, SetLevel = 3, UnionMax = 4, CarveMin = 5 });
u8_enum!(Profile { Chamfer = 0, Round = 1, Smooth = 2, HardStep = 3 });
u8_enum!(Join { Layer = 0, Union = 1, Subtract = 2, Intersect = 3, Pocket = 4, Slash = 5, ClipDivide = 6 });

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Shape {
    #[serde(rename = "t")]   pub kind: ShapeKind,
    #[serde(rename = "m")]   pub mode: Mode,
    pub x: f32, pub y: f32, pub w: f32, pub h: f32,
    #[serde(rename = "r")]   pub rot_deg: f32,
    #[serde(rename = "rad")] pub corner: f32,
    #[serde(rename = "bev")] pub bevel: f32,
    #[serde(rename = "H")]   pub height: f32,
    #[serde(rename = "pf")]  pub profile: Profile,
    #[serde(rename = "n")]   pub sides: f32,
    #[serde(rename = "j")]   pub join: Join,
    #[serde(rename = "sm")]  pub blend: f32,
    pub p1: f32,
    pub p2: f32,
}

impl Default for Shape {
    fn default() -> Self {
        Self { kind: ShapeKind::Rect, mode: Mode::Raise, x: 512.0, y: 512.0, w: 300.0, h: 200.0,
               rot_deg: 0.0, corner: 20.0, bevel: 24.0, height: 20.0, profile: Profile::Chamfer,
               sides: 6.0, join: Join::Layer, blend: 0.0, p1: 0.0, p2: 0.0 }
    }
}

#[derive(Serialize, Deserialize)]
pub struct Project { pub shapes: Vec<Shape> }
```

---

## 4. The evaluation model (existing behaviour)

`height(p)` walks the shape list from first to last. State:

| Variable | Meaning |
|---|---|
| `h` | Running height at `p` |
| `head` | The layer currently being built: its first shape supplies mode, height, bevel and profile |
| `acc` | Combined signed distance of the head and everything unioned, subtracted or intersected into it |
| `hb` | Height **before** the head layer was applied. Pocket and Slash never cut below it |
| `ex` | **New.** Extra height added to the head layer at this pixel (used by Clip Divide steps) |

Per shape, in order:

```
Pocket      -> flush head, then cut down to a floor of hb
Slash       -> flush head, then cut a tapered groove down to a floor of hb          (new)
ClipDivide  -> if a head exists, split its outline (and optionally step it)         (new)
Layer       -> flush head, start a new head
Union / Subtract / Intersect
            -> if no head exists, behave like Layer; otherwise modify acc
end of list -> flush head
```

"Flush" means `h = apply(h, acc, head, ex)`.

Pocket and Slash end the current head. Union, Subtract and Intersect after them start a new layer. This is existing behaviour; keep it.

---

## 5. Shared math

### 5.1 Profile

```
pf(t, k):   k = 0 -> t                              (chamfer)
            k = 1 -> sqrt(max(1 - (1 - t)^2, 0))     (round)
            k = 2 -> t * t * (3 - 2t)                (smooth)
```
HardStep (3) is mapped to `k = 2` with the wall width forced to 1 unit, **except** in Groove mode, where the width is kept. Wall parameters:

```
wall(shape, keep_width):  w = max(bevel, 0.5);  k = profile
                          if k == 3 { k = 2; if !keep_width { w = 1.0 } }
```

### 5.2 Smooth minimum

```
smin(a, b, k) = if k < 0.01 { min(a, b) }
                else { let h = max(k - |a - b|, 0) / k;  min(a, b) - h * h * k * 0.25 }
```

### 5.3 Apply (flush) a layer

```
apply(h, s, head, ex):
    H = head.height + ex
    (w, k) = wall(head, head.mode == Groove)
    t = pf(clamp(-s / w, 0, 1), k)
    Raise    -> h + H * t
    Cut      -> h - H * t
    Groove   -> h - H * pf(1 - clamp(|s| / w, 0, 1), k)
    SetLevel -> mix(h, H, t)
    UnionMax -> max(h, H * t)
    CarveMin -> min(h, -H * t)
```
With `ex = 0` this is identical to the prototype.

---

## 6. Operations

### 6.1 Partial Cut (Pocket, `j = 4`), existing

**Purpose.** Cut a recessed panel into the previous layer. The cut can be shallower than the layer, leaving a floor, and can never go below the layer's base.

**Parameters** (all from the pocket shape itself)

| Field | Use |
|---|---|
| `kind`, `w`, `h`, `x`, `y`, `r`, `corner`, `sides` | Outline of the pocket (any shape kind) |
| `height` | Pocket depth, 0 to 96 |
| `bevel` | Wall width |
| `profile` | Wall profile |

**Math**

```
(w, k) = wall(pocket, keep_width = false)
s      = sdf(p, pocket)
h      = max( h - pocket.height * pf(clamp(-s / w, 0, 1), k),  hb )
```
Before this, if a head exists it is flushed and cleared.

**Behaviour to verify**

* Depth 10 into a plate of height 24 leaves a floor at 14.
* Depth 40 into a plate of height 24 cuts exactly to the plate base (`hb`), not below.
* Where the pocket overhangs the plate, nothing changes (the floor `hb` is already the height there).
* Multiple pockets after one plate each get independent depth, walls and profile.

### 6.2 Partial Slash (`j = 5`), new

**Purpose.** A straight blade-style groove with a rounded cross-section, rounded ends and optional length taper.

**Shape fields used**

| Field | Use |
|---|---|
| `x`, `y`, `r` | Centre and direction. The slash runs along the local X axis |
| `w` | Total length (end to end, including rounded caps) |
| `h` | Total width of the groove |
| `height` | Maximum depth, 0 to 96 |
| `profile` | Cross-section: Chamfer is a V groove, Round is a semicircular groove, Smooth is a soft U, HardStep is a flat-bottom channel with a 1-unit wall |
| `p1` | Taper-in fraction of the length (0 to 0.5). 0 means no taper |
| `p2` | Taper-out fraction (0 to 0.5). 0 means no taper |

`kind`, `corner`, `sides`, `bevel` and `blend` are ignored for a slash. The UI should lock `kind` to Rect and hide the ignored controls.

**Math**

```
q        = local(p, slash)
hw       = max(h / 2, 0.5)                 // half width
half_len = w / 2
cap      = max(half_len - hw, 0)           // straight section half-length
r        = length( vec2( max(|q.x| - cap, 0), q.y ) )     // distance to the core segment (a capsule)
u        = clamp( (q.x + half_len) / max(w, 1), 0, 1 )    // 0 at start, 1 at end

taper    = 1
if p1 > 0.001 { taper *= smoothstep(0, min(p1, 0.5), u) }
if p2 > 0.001 { taper *= smoothstep(0, min(p2, 0.5), 1 - u) }

cross    = if profile == HardStep { clamp(hw - r, 0, 1) }
           else                    { pf(1 - clamp(r / hw, 0, 1), profile) }

cut      = height * taper * cross
h        = max(h - cut, hb)
```
Before this, flush and clear any head (same as Pocket).

**Behaviour to verify**

* With `p1 = p2 = 0`, depth at the centreline equals `height` along the whole straight section.
* With `p1 = 0.25`, depth at `u = 0` is 0 and reaches `height` at `u = 0.25`.
* Cut depth never exceeds `height`, and the result never drops below `hb`.
* Rotation by `r` rotates the slash around its centre.
* `w < h` degenerates to a circular dimple of radius `hw`; it must not produce NaN.

### 6.3 Clip Divide (`j = 6`), new

**Purpose.** Split the current layer with a straight seam so both pieces get their own bevel along the seam. Optionally step one half.

**Applies to** the current head layer (the most recent Layer, plus anything unioned, subtracted or intersected into it). **If no head exists** (for example directly after a Pocket or Slash) the shape is ignored. The UI must show a warning in that case.

**Shape fields used**

| Field | Use |
|---|---|
| `x`, `y`, `r` | A point on the seam and its direction. The seam runs along local X. "Side B" is local `+Y` |
| `h` | Gap width of the seam, minimum 1 |
| `w` | Seam length. Used only when finite |
| `blend` | Corner softening where the seam meets the layer outline or ends |
| `p1` | Step height applied to side B, signed. Only for infinite divides |
| `p2` | `1` = infinite, `0` = finite (partial divide) |

The bevel width, profile and mode of the seam walls come from the head layer, exactly like Subtract.

**Math**

```
q    = local(p, divider)
gap  = max(h, 1)
band = |q.y| - gap / 2                              // negative inside the seam
if p2 < 0.5 { band = max(band, |q.x| - w / 2) }     // finite seam: a rectangle

acc  = -smin(-acc, band, blend)                     // identical form to Subtract with s = band

if p2 >= 0.5 { ex += p1 * smoothstep(-0.5, 0.5, q.y) }   // step side B only
```
`ex` is reset to 0 whenever a new head starts and is passed to `apply` when the head is flushed.

**Why the step has no visible seam artefact.** At the centre of the gap, `acc > 0`, so the head's contribution `t` is 0. The step only scales height where the layer exists, so there is no discontinuity.

**Behaviour to verify**

* Infinite divide with gap 12 and bevel 8 gives two pieces, each with a bevel on the seam side.
* Finite divide stops short and the layer stays connected around the end.
* Step `+6` raises side B by 6 relative to side A for Raise layers; Set level layers shift their target level by 6.
* Two dividers on one layer accumulate: `ex` sums, and `acc` loses both bands.
* A divider with no head is a no-op (no panic, no change to `h`).

---

## 7. Rust core crate (CPU reference)

Put this in a crate with no Bevy dependency (`normal_forge_core`) so it is testable headless. Section 7 shows the code for the evaluator; the types are in Section 3.3.

```rust
use glam::{Vec2, Vec3};

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
fn glsl_mod(x: f32, y: f32) -> f32 { x - y * (x / y).floor() }   // NOT Rust's % operator

pub fn pf(t: f32, k: u8) -> f32 {
    match k {
        0 => t,
        1 => (1.0 - (1.0 - t) * (1.0 - t)).max(0.0).sqrt(),
        _ => t * t * (3.0 - 2.0 * t),
    }
}

pub fn smin(a: f32, b: f32, k: f32) -> f32 {
    if k < 0.01 { return a.min(b); }
    let h = (k - (a - b).abs()).max(0.0) / k;
    a.min(b) - h * h * k * 0.25
}

pub fn local(p: Vec2, s: &Shape) -> Vec2 {
    let d = p - Vec2::new(s.x, s.y);
    let (sn, cs) = s.rot_deg.to_radians().sin_cos();
    Vec2::new(cs * d.x + sn * d.y, -sn * d.x + cs * d.y)
}

pub fn sdf(p: Vec2, s: &Shape) -> f32 {
    let q = local(p, s);
    let hf = Vec2::new(s.w, s.h) * 0.5;
    match s.kind {
        ShapeKind::Rect => {
            let r = s.corner.min(hf.x.min(hf.y));
            let e = q.abs() - hf + Vec2::splat(r);
            e.max(Vec2::ZERO).length() + e.x.max(e.y).min(0.0) - r
        }
        ShapeKind::Ellipse => {
            let k0 = (q / hf).length();
            let k1 = (q / (hf * hf)).length();
            if k1 < 1e-6 { -hf.x.min(hf.y) } else { k0 * (k0 - 1.0) / k1 }
        }
        ShapeKind::Polygon => {
            let big_r = hf.x.min(hf.y);
            let an = std::f32::consts::PI / s.sides;
            let bn = glsl_mod(q.x.atan2(q.y), 2.0 * an) - an;      // note atan2(x, y) argument order
            let mut t = Vec2::new(bn.cos(), bn.sin().abs()) * q.length()
                      - big_r * Vec2::new(an.cos(), an.sin());
            t.y += (-t.y).clamp(0.0, big_r * an.sin());
            t.length() * t.x.signum()
        }
    }
}

fn wall(s: &Shape, keep_width: bool) -> (f32, u8) {
    let mut w = s.bevel.max(0.5);
    let mut k = s.profile as u8;
    if k == 3 { k = 2; if !keep_width { w = 1.0; } }
    (w, k)
}

fn apply(h: f32, sd: f32, head: &Shape, ex: f32) -> f32 {
    let hh = head.height + ex;
    let (w, k) = wall(head, head.mode == Mode::Groove);
    let t = pf((-sd / w).clamp(0.0, 1.0), k);
    match head.mode {
        Mode::Raise    => h + hh * t,
        Mode::Cut      => h - hh * t,
        Mode::Groove   => h - hh * pf(1.0 - (sd.abs() / w).clamp(0.0, 1.0), k),
        Mode::SetLevel => h + (hh - h) * t,
        Mode::UnionMax => h.max(hh * t),
        Mode::CarveMin => h.min(-hh * t),
    }
}

fn slash_cut(p: Vec2, s: &Shape) -> f32 {
    let q = local(p, s);
    let hw = (s.h * 0.5).max(0.5);
    let half_len = s.w * 0.5;
    let cap = (half_len - hw).max(0.0);
    let r = Vec2::new((q.x.abs() - cap).max(0.0), q.y).length();
    let u = ((q.x + half_len) / s.w.max(1.0)).clamp(0.0, 1.0);
    let mut taper = 1.0;
    if s.p1 > 0.001 { taper *= smoothstep(0.0, s.p1.min(0.5), u); }
    if s.p2 > 0.001 { taper *= smoothstep(0.0, s.p2.min(0.5), 1.0 - u); }
    let cross = if s.profile == Profile::HardStep {
        (hw - r).clamp(0.0, 1.0)
    } else {
        pf(1.0 - (r / hw).clamp(0.0, 1.0), s.profile as u8)
    };
    s.height * taper * cross
}

pub fn height(p: Vec2, shapes: &[Shape]) -> f32 {
    let (mut h, mut acc, mut hb, mut ex) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    let mut head: Option<&Shape> = None;

    for s in shapes.iter().take(MAX_SHAPES) {
        let sd = sdf(p, s);
        match s.join {
            Join::Pocket => {                                              // existing
                if let Some(hd) = head.take() { h = apply(h, acc, hd, ex); }
                let (w, k) = wall(s, false);
                h = (h - s.height * pf((-sd / w).clamp(0.0, 1.0), k)).max(hb);
            }
            Join::Slash => {                                               // new
                if let Some(hd) = head.take() { h = apply(h, acc, hd, ex); }
                h = (h - slash_cut(p, s)).max(hb);
            }
            Join::ClipDivide => {                                          // new
                if head.is_some() {
                    let q = local(p, s);
                    let gap = s.h.max(1.0);
                    let infinite = s.p2 >= 0.5;
                    let mut band = q.y.abs() - gap * 0.5;
                    if !infinite { band = band.max(q.x.abs() - s.w * 0.5); }
                    acc = -smin(-acc, band, s.blend);
                    if infinite { ex += s.p1 * smoothstep(-0.5, 0.5, q.y); }
                }
            }
            _ => {                                                         // Layer, Union, Subtract, Intersect
                if s.join == Join::Layer || head.is_none() {
                    if let Some(hd) = head { h = apply(h, acc, hd, ex); }
                    hb = h; acc = sd; head = Some(s); ex = 0.0;
                } else {
                    match s.join {
                        Join::Union     => acc = smin(acc, sd, s.blend),
                        Join::Subtract  => acc = -smin(-acc, sd, s.blend),
                        Join::Intersect => acc = -smin(-acc, -sd, s.blend),
                        _ => unreachable!(),
                    }
                }
            }
        }
    }
    if let Some(hd) = head { h = apply(h, acc, hd, ex); }
    h
}

pub fn normal(p: Vec2, shapes: &[Shape], e: f32, strength: f32) -> Vec3 {
    let gx = (height(p + Vec2::X * e, shapes) - height(p - Vec2::X * e, shapes)) / (2.0 * e);
    let gy = (height(p + Vec2::Y * e, shapes) - height(p - Vec2::Y * e, shapes)) / (2.0 * e);
    Vec3::new(-gx * strength, gy * strength, 1.0).normalize()
}
```

A full-image CPU renderer loops pixels and `aa × aa` sub-samples, averages `normal()`, flips `y` for DirectX, and encodes to RGBA8. Use `rayon` for parallel rows.

---

## 8. WGSL shader (GPU)

File: `assets/shaders/normal_forge.wgsl`. Mechanical port of the prototype's GLSL, plus the two new branches.

**Port pitfalls**

| GLSL | WGSL | Note |
|---|---|---|
| `mod(x, y)` | `x - y * floor(x / y)` | WGSL `%` on floats truncates toward zero and differs for negatives |
| `atan(a, b)` | `atan2(a, b)` | Same argument order, so `atan2(q.x, q.y)` stays as is |
| `gl_FragCoord.y` (bottom-left) | `@builtin(position).y` (top-left) | Remove the Y flip |
| `uniform vec4 uA[32]` | `array<vec4<f32>, 32>` in a uniform struct | Stride 16, so no padding issue |
| `int` join compare | `u32(d.x + 0.5)` | Never use `> 3.5` |

```wgsl
struct Params {
    res: f32,
    strength: f32,
    aa: u32,
    n: u32,
    view: u32,
    dx: u32,
    light: vec2<f32>,
    tile_origin: vec2<f32>,
    _pad: vec2<f32>,
    a: array<vec4<f32>, 32>,   // x, y, w, h
    b: array<vec4<f32>, 32>,   // rot (radians), corner, bevel, height
    c: array<vec4<f32>, 32>,   // kind, mode, profile, sides
    d: array<vec4<f32>, 32>,   // join, blend, p1, p2
}
@group(2) @binding(0) var<uniform> params: Params;   // group index depends on Bevy version: check Material2d docs

fn local_q(p: vec2<f32>, a: vec4<f32>, rot: f32) -> vec2<f32> {
    let dd = p - a.xy;
    let cs = cos(rot); let sn = sin(rot);
    return vec2<f32>(cs * dd.x + sn * dd.y, -sn * dd.x + cs * dd.y);
}

fn glsl_mod(x: f32, y: f32) -> f32 { return x - y * floor(x / y); }

fn sdf(p: vec2<f32>, a: vec4<f32>, b: vec4<f32>, c: vec4<f32>) -> f32 {
    let q = local_q(p, a, b.x);
    let hf = a.zw * 0.5;
    if (c.x < 0.5) {
        let r = min(b.y, min(hf.x, hf.y));
        let e = abs(q) - hf + vec2<f32>(r);
        return length(max(e, vec2<f32>(0.0))) + min(max(e.x, e.y), 0.0) - r;
    }
    if (c.x < 1.5) {
        let k0 = length(q / hf);
        let k1 = length(q / (hf * hf));
        if (k1 < 1e-6) { return -min(hf.x, hf.y); }
        return k0 * (k0 - 1.0) / k1;
    }
    let big_r = min(hf.x, hf.y);
    let an = 3.14159265 / c.w;
    let bn = glsl_mod(atan2(q.x, q.y), 2.0 * an) - an;
    var t = length(q) * vec2<f32>(cos(bn), abs(sin(bn))) - big_r * vec2<f32>(cos(an), sin(an));
    t.y = t.y + clamp(-t.y, 0.0, big_r * sin(an));
    return length(t) * sign(t.x);
}

fn pf(t: f32, k: f32) -> f32 {
    if (k < 0.5) { return t; }
    if (k < 1.5) { return sqrt(max(1.0 - (1.0 - t) * (1.0 - t), 0.0)); }
    return t * t * (3.0 - 2.0 * t);
}

fn smn(a: f32, b: f32, k: f32) -> f32 {
    if (k < 0.01) { return min(a, b); }
    let h = max(k - abs(a - b), 0.0) / k;
    return min(a, b) - h * h * k * 0.25;
}

fn app(h: f32, s: f32, b: vec4<f32>, c: vec4<f32>, ex: f32) -> f32 {
    let hh = b.w + ex;
    var w = max(b.z, 0.5);
    var k = c.z;
    let m = c.y;
    if (k > 2.5) { k = 2.0; if (m != 2.0) { w = 1.0; } }
    let t = pf(clamp(-s / w, 0.0, 1.0), k);
    if (m < 0.5) { return h + hh * t; }
    if (m < 1.5) { return h - hh * t; }
    if (m < 2.5) { return h - hh * pf(1.0 - clamp(abs(s) / w, 0.0, 1.0), k); }
    if (m < 3.5) { return mix(h, hh, t); }
    if (m < 4.5) { return max(h, hh * t); }
    return min(h, -hh * t);
}

fn slash_cut(p: vec2<f32>, a: vec4<f32>, b: vec4<f32>, c: vec4<f32>, d: vec4<f32>) -> f32 {
    let q = local_q(p, a, b.x);
    let hw = max(a.w * 0.5, 0.5);
    let half_len = a.z * 0.5;
    let cap = max(half_len - hw, 0.0);
    let r = length(vec2<f32>(max(abs(q.x) - cap, 0.0), q.y));
    let u = clamp((q.x + half_len) / max(a.z, 1.0), 0.0, 1.0);
    var taper = 1.0;
    if (d.z > 0.001) { taper = taper * smoothstep(0.0, min(d.z, 0.5), u); }
    if (d.w > 0.001) { taper = taper * smoothstep(0.0, min(d.w, 0.5), 1.0 - u); }
    var cross: f32;
    if (c.z > 2.5) { cross = clamp(hw - r, 0.0, 1.0); }
    else { cross = pf(1.0 - clamp(r / hw, 0.0, 1.0), c.z); }
    return b.w * taper * cross;
}

fn ht(p: vec2<f32>) -> f32 {
    var h = 0.0; var acc = 0.0; var hb = 0.0; var ex = 0.0;
    var pb = vec4<f32>(0.0); var pc = vec4<f32>(0.0);
    var has = false;
    for (var i = 0u; i < params.n; i = i + 1u) {
        let a = params.a[i]; let b = params.b[i]; let c = params.c[i]; let d = params.d[i];
        let j = u32(d.x + 0.5);
        let s = sdf(p, a, b, c);
        if (j == 4u) {                                   // POCKET (existing)
            if (has) { h = app(h, acc, pb, pc, ex); has = false; }
            var w = max(b.z, 0.5); var k = c.z;
            if (k > 2.5) { k = 2.0; w = 1.0; }
            h = max(h - b.w * pf(clamp(-s / w, 0.0, 1.0), k), hb);
        } else if (j == 5u) {                            // SLASH (new)
            if (has) { h = app(h, acc, pb, pc, ex); has = false; }
            h = max(h - slash_cut(p, a, b, c, d), hb);
        } else if (j == 6u) {                            // CLIP DIVIDE (new)
            if (has) {
                let q = local_q(p, a, b.x);
                let gap = max(a.w, 1.0);
                let inf = d.w > 0.5;
                var band = abs(q.y) - gap * 0.5;
                if (!inf) { band = max(band, abs(q.x) - a.z * 0.5); }
                acc = -smn(-acc, band, d.y);
                if (inf) { ex = ex + d.z * smoothstep(-0.5, 0.5, q.y); }
            }
        } else if (j == 0u || !has) {                    // LAYER, or a join with no head
            if (has) { h = app(h, acc, pb, pc, ex); }
            hb = h; acc = s; pb = b; pc = c; has = true; ex = 0.0;
        } else if (j == 1u) { acc = smn(acc, s, d.y); }
        else if (j == 2u) { acc = -smn(-acc, s, d.y); }
        else { acc = -smn(-acc, -s, d.y); }
    }
    if (has) { h = app(h, acc, pb, pc, ex); }
    return h;
}
```

The fragment entry point does the AA loop, finite differences and view modes (Normal, Height, Lit) exactly as the prototype does. Use `frag = in.position.xy + params.tile_origin` and `p = vec2(frag.x + o.x, frag.y + o.y) * 1024.0 / params.res`.

---

## 9. Bevy integration

Target the Bevy version you have pinned. Render and readback APIs (`Camera` render targets, `Readback`, event or observer types, material bind group indices) were renamed in several recent releases, so confirm names against that version's examples (`render_to_texture`, `gpu_readback`, `shader_material_2d`). The algorithm does not depend on the version.

### 9.1 Plugin layout

```
normal_forge_core/          pure Rust: types, sdf, height, normal, CPU renderer, tests
normal_forge_bevy/
  src/lib.rs                NormalForgePlugin
  src/gpu.rs                GpuParams, pack(), material
  src/ui.rs                 bevy_egui panels + gizmo overlay
  src/export.rs             tiled render + readback + PNG
  assets/shaders/normal_forge.wgsl
```

### 9.2 Uniform packing

```rust
use bevy::{prelude::*, render::render_resource::ShaderType};

#[derive(ShaderType, Clone, Copy)]
pub struct GpuParams {
    pub res: f32, pub strength: f32, pub aa: u32, pub n: u32,
    pub view: u32, pub dx: u32, pub light: Vec2,
    pub tile_origin: Vec2, pub _pad: Vec2,
    pub a: [Vec4; 32], pub b: [Vec4; 32], pub c: [Vec4; 32], pub d: [Vec4; 32],
}

pub fn pack(shapes: &[Shape], p: &mut GpuParams) {
    p.n = shapes.len().min(MAX_SHAPES) as u32;
    for (i, s) in shapes.iter().take(MAX_SHAPES).enumerate() {
        p.a[i] = Vec4::new(s.x, s.y, s.w, s.h);
        p.b[i] = Vec4::new(s.rot_deg.to_radians(), s.corner, s.bevel, s.height);
        p.c[i] = Vec4::new(s.kind as u8 as f32, s.mode as u8 as f32, s.profile as u8 as f32, s.sides);
        p.d[i] = Vec4::new(s.join as u8 as f32, s.blend, s.p1, s.p2);
    }
}
```
Uniform size is 4 × 32 × 16 = 2 KB plus a small header, well under every backend limit.

### 9.3 Material and render target

```rust
#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub struct NormalForgeMaterial {
    #[uniform(0)]
    pub params: GpuParams,
}

impl Material2d for NormalForgeMaterial {
    fn fragment_shader() -> ShaderRef { "shaders/normal_forge.wgsl".into() }
}
```

Render a full-target quad with an orthographic `Camera2d` into an `Image` render target. These settings are **mandatory for correct normals**:

* Texture format `Rgba8Unorm`. **Not** `Rgba8UnormSrgb`: an sRGB target would gamma-encode the normal values and tilt every normal.
* Usage flags: `TEXTURE_BINDING | COPY_SRC | RENDER_ATTACHMENT`.
* MSAA off, tonemapping off, no bloom, no post-processing, no HDR on that camera.
* Preview target 1024², `aa = 2`. Activate the camera for a single frame when the project changes; do not re-render every frame.

### 9.4 Tiled export and readback

Large exports (4096² with 3×3 AA) can exceed GPU watchdog limits in a single draw, so render in tiles:

1. Choose `tile = 512`. Create one `tile × tile` target.
2. For each tile `(tx, ty)`: set `params.res = full_res`, `params.tile_origin = (tx * tile, ty * tile)`, render once, read back.
3. Copy the tile rows into a `full_res × full_res × 4` CPU buffer at the right offset. The row order from readback is top to bottom, matching the shader's top-left origin.
4. Write the PNG with the `image` crate as 8-bit RGBA with no colour-space chunk (or an explicit linear/unspecified profile).
5. For a height export set `params.view = 1`; encoding is `0.5 + h / 128`, giving a ±64 unit range.

Wait for each readback to complete before starting the next tile; run the tiles as an async task or a small state machine in a `Resource`.

---

## 10. UI changes (additive)

Use `bevy_egui` for panels and Bevy `Gizmos` for the on-canvas overlay.

1. **Combine dropdown** gains two entries at the end: `Slash (partial length)` and `Clip divide`. Existing entries keep their order and values.
2. **Per-join property panels**

   | Join | Controls shown |
   |---|---|
   | Pocket (4) | Wall profile, Pocket depth, Wall bevel width, plus shape geometry |
   | Slash (5) | Profile, Depth, Length (`w`), Width (`h`), Taper in (`p1`), Taper out (`p2`), position, rotation. Kind is locked to Rect |
   | ClipDivide (6) | Gap (`h`), Blend, Infinite toggle (`p2`), Length (`w`, only if finite), Step (`p1`, only if infinite), position, rotation |
3. **Layer list markers**: existing `∪ − ∩ ▽`; add `╱` for Slash and `⫽` for Clip Divide.
4. **Gizmos**: slash draws its capsule and two taper handles on the centreline; clip divide draws the seam line, an arrow pointing to side B, and a length handle when finite.
5. **Warnings**: a ClipDivide with no head layer shows "Clip Divide needs a layer, union, subtract or intersect before it". A project with more than 32 shapes shows a limit message and refuses the 33rd.
6. **Reorder** must still force `shapes[0].join = Layer`, as in the prototype, because the first shape cannot join.

---

## 11. Testing and acceptance criteria

### 11.1 Golden regression (do this first)

1. Export `demo_v1.json` and reference PNGs from the web prototype: normal and height, 512², AA off, OpenGL. Commit them under `tests/golden/`.
2. Test: CPU render of `demo_v1.json` with the new code must match the PNG with **max per-channel difference ≤ 2/255** and mean difference ≤ 0.2/255.
3. Test: the same for the GPU path, read back, with max difference ≤ 3/255.
4. These tests must pass before any new operation is added and again after.

### 11.2 Property tests for the new operations

```rust
#[test] fn pocket_never_below_base() {
    // plate H=24 at (512,512), pocket depth 40 over it: height inside pocket == 0.0 within 1e-4
}
#[test] fn pocket_partial_leaves_floor() {
    // plate H=24, pocket depth 10: centre of pocket == 14.0 within 1e-3
}
#[test] fn slash_depth_and_floor() {
    // no taper: centreline depth == H; never cuts below hb
}
#[test] fn slash_taper_endpoints() {
    // p1=0.25: depth at u=0 is 0; at u=0.25 equals H
}
#[test] fn slash_degenerate_no_nan() {
    // w < h and w = 0: all samples finite
}
#[test] fn divide_splits_outline() {
    // infinite divide: height at seam centre == base; both sides raised
}
#[test] fn divide_step_offsets_side_b() {
    // Raise layer H=20, step +6: plateau on side B == 26, side A == 20
}
#[test] fn divide_without_head_is_noop() {
    // pocket followed by divide: identical to pocket alone
}
#[test] fn old_project_defaults() {
    // JSON without p1/p2 deserializes; p1 == p2 == 0.0
}
#[test] fn unknown_join_rejected_cleanly() {
    // j = 9 returns an error, never panics
}
```

### 11.3 CPU and GPU parity

Render one scene containing every mode, every join (0 to 6), every profile and every shape kind. CPU and GPU outputs must agree within 3/255 per channel at AA = 1, and within 2/255 at AA = 3.

### 11.4 Acceptance checklist

- [ ] Golden regression passes before and after the change.
- [ ] Old project JSON loads and renders identically.
- [ ] Pocket matches the behaviours in 6.1.
- [ ] Slash matches the behaviours in 6.2.
- [ ] Clip Divide matches the behaviours in 6.3.
- [ ] No NaN or infinity anywhere, including zero-size shapes.
- [ ] 4096² export with AA 3 completes without a GPU timeout (tiled).
- [ ] Output texture is linear (`Rgba8Unorm`) and the saved PNG has no gamma shift.
- [ ] OpenGL and DirectX Y outputs are exact mirrors in the green channel.

---

## 12. Suggested implementation order

1. Create the core crate with types, serde, `sdf`, `pf`, `smin`, `apply`, and the **existing** `height` (joins 0 to 4 only). Add the golden tests. Get them green.
2. Port the same logic to WGSL and wire the Bevy material. Get the GPU golden test green.
3. Add `p1`, `p2` and join values 5 and 6 to the types (still no behaviour change). Re-run goldens.
4. Add Slash to the CPU evaluator with its tests, then to WGSL, then the CPU/GPU parity test.
5. Add Clip Divide the same way (CPU, tests, WGSL, parity).
6. Add UI panels, gizmos and warnings.
7. Add tiled export and readback.

## 13. Known limits and open questions

* **Pocket, Slash and Clip Divide only see the most recent head layer.** An unrelated layer between a plate and a later Pocket or Slash means the floor `hb` belongs to the in-between layer. A future `target_layer` index field could lift this.
* **One divider per side of a step** is supported by summing `ex`; a divider with a non-axis-aligned partial overlap onto another divider is not specially handled.
* **Polygon `sides` is stored as a float** to match the prototype's JSON; round to an integer in the UI.
* **Confirm the interpretation of "partial slash" and "clip divide"** (see Section 0) before building the UI. The evaluator sections are small and isolated, so changing the definition later is cheap.
* The web prototype does not yet implement joins 5 and 6. If you want parity, port the same two branches to its GLSL `ht()` function, taking care with the `d.x > 3.5` pocket test described in Section 1.
