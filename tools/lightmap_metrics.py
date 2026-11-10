#!/usr/bin/env python3
"""Artifact metrics for baked lightmaps (PNG 8/16-bit). Needs numpy + Pillow.

  python lightmap_metrics.py a.png [b.png ...]

banding_run_p95  95th percentile length (texels) of horizontal runs of one 8-bit
                 grey value inside smooth, covered regions. Posterized gradients
                 have long runs; a well-dithered gradient has short ones.
hf_noise         std-dev of (luminance - 5x5 box blur) over smooth covered regions.
halo_ratio       mean luminance of the 1-texel ring just inside the uncovered
                 border divided by the ring 4 texels in. Needs real transparency
                 (alpha 0 outside the island gutters); ~1.0 is clean.
"""
import sys
import numpy as np
from PIL import Image

def load(path):
    im = Image.open(path)
    a = np.asarray(im).astype(np.float64)
    scale = 65535.0 if a.max() > 255 else 255.0
    if a.ndim == 2:
        rgb, alpha = np.repeat(a[..., None], 3, -1) / scale, np.ones(a.shape)
    else:
        rgb = a[..., :3] / scale
        alpha = a[..., 3] / scale if a.shape[2] == 4 else np.ones(a.shape[:2])
    return rgb, alpha > 0.5

def box(a, k):
    p = k // 2
    pad = np.pad(a, p, mode="edge")
    out = np.zeros_like(a)
    for dy in range(k):
        for dx in range(k):
            out += pad[dy:dy + a.shape[0], dx:dx + a.shape[1]]
    return out / (k * k)

def metrics(path):
    rgb, cov = load(path)
    lum = 0.2126 * rgb[..., 0] + 0.7152 * rgb[..., 1] + 0.0722 * rgb[..., 2]
    blur = box(lum, 5)
    smooth = cov & (np.abs(lum - blur) < 0.02)
    smooth &= (box(cov.astype(float), 7) > 0.999)           # stay away from borders
    # banding: run lengths of identical 8-bit grey inside smooth areas
    g8 = np.round(lum * 255).astype(np.int32)
    runs = []
    for y in range(g8.shape[0]):
        row, m = g8[y], smooth[y]
        x = 0
        while x < len(row):
            if not m[x]: x += 1; continue
            x0 = x
            while x < len(row) and m[x] and row[x] == row[x0]: x += 1
            runs.append(x - x0)
    p95 = float(np.percentile(runs, 95)) if runs else float("nan")
    hf = float(np.std((lum - blur)[smooth])) if smooth.any() else float("nan")
    # halo ratio
    inner = cov & ~(box(cov.astype(float), 3) > 0.999)       # 1-texel ring inside
    deep = cov & (box(cov.astype(float), 9) > 0.999)
    halo = float(lum[inner].mean() / max(lum[deep].mean(), 1e-9)) if inner.any() and deep.any() else float("nan")
    return p95, hf, halo

if __name__ == "__main__":
    print(f"{'file':32s} {'banding_run_p95':>16s} {'hf_noise':>10s} {'halo_ratio':>11s}")
    for p in sys.argv[1:]:
        b, n, h = metrics(p)
        print(f"{p[-32:]:32s} {b:16.1f} {n:10.5f} {h:11.3f}")
