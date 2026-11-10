#!/usr/bin/env python3
"""Turns the captured UI stills into a captioned 1080p30 tutorial video.

Each step in steps.json becomes a shot: the cursor glides to the action, a
click pulse fires, the previous still crossfades into the new one and the camera
pushes into the focus rectangle. Frames are piped straight into ffmpeg, so no
intermediate PNGs are written.

Usage:
    python tools/tutorial_compose.py
    python tools/tutorial_compose.py --steps output/tutorial/final/steps.json
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from collections import OrderedDict
from dataclasses import dataclass, field
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parent.parent
FFMPEG_CANDIDATES = [
    Path(r"C:\Program Files\Shotcut\ffmpeg.exe"),
    Path("ffmpeg"),
]
OUT_SIZE = (1920, 1080)
FPS = 30

# Palette lifted from the app's own theme so the captions feel native.
BG = (14, 15, 19)
CARD = (26, 28, 35)
CARD_EDGE = (58, 62, 74)
TEXT = (235, 237, 242)
MUTED = (156, 163, 178)
ACCENT = (235, 152, 52)
SCRIM = (10, 11, 14)

FONT_DIR = ROOT / "assets" / "fonts"


def font(name: str, size: int) -> ImageFont.FreeTypeFont:
    return ImageFont.truetype(str(FONT_DIR / name), size)


# --- geometry -------------------------------------------------------------


@dataclass
class Cam:
    """A camera: the crop window in still pixels."""

    x: float
    y: float
    w: float
    h: float

    def lerp(self, other: "Cam", t: float) -> "Cam":
        return Cam(
            self.x + (other.x - self.x) * t,
            self.y + (other.y - self.y) * t,
            self.w + (other.w - self.w) * t,
            self.h + (other.h - self.h) * t,
        )


def full_cam(size: tuple[int, int]) -> Cam:
    return Cam(0.0, 0.0, float(size[0]), float(size[1]))


def points_rect(points, pad: float = 260.0):
    """Bounding box around the pointer path, padded, for shots without a focus."""
    if not points:
        return None
    xs = [float(point[0]) for point in points]
    ys = [float(point[1]) for point in points]
    left, right = min(xs) - pad, max(xs) + pad
    top, bottom = min(ys) - pad, max(ys) + pad
    return (max(0.0, left), max(0.0, top), right - left, bottom - top)


def focus_cam(size: tuple[int, int], rect, max_scale: float = 1.85) -> Cam:
    """Crop that frames *rect* (client pixels) with a 16:9 aspect."""
    width, height = float(size[0]), float(size[1])
    if not rect:
        return Cam(0.0, 0.0, width, height)
    x, y, w, h = (float(value) for value in rect)
    scale = min(max_scale, max(1.0, 0.8 * height / max(h, 1.0)))
    crop_h = height / scale
    crop_w = crop_h * (16.0 / 9.0)
    if crop_w > width:
        crop_w = width
        crop_h = crop_w * (9.0 / 16.0)
    cx, cy = x + w / 2.0, y + h / 2.0
    left = min(max(cx - crop_w / 2.0, 0.0), width - crop_w)
    top = min(max(cy - crop_h / 2.0, 0.0), height - crop_h)
    return Cam(left, top, crop_w, crop_h)


def ease(t: float) -> float:
    """smoothstep, clamped."""
    t = min(max(t, 0.0), 1.0)
    return t * t * (3.0 - 2.0 * t)


# --- still cache ----------------------------------------------------------


class Stills:
    """Small LRU of decoded stills; the frames come from disk on demand."""

    def __init__(self, limit: int = 4):
        self._cache: OrderedDict[Path, Image.Image] = OrderedDict()
        self._limit = limit

    def image(self, path: Path) -> Image.Image:
        if path in self._cache:
            self._cache.move_to_end(path)
            return self._cache[path]
        image = Image.open(path).convert("RGB")
        self._cache[path] = image
        if len(self._cache) > self._limit:
            self._cache.popitem(last=False)
        return image

    def size(self, path: Path) -> tuple[int, int]:
        return self.image(path).size


def render_still(stills: Stills, path: Path, cam: Cam) -> Image.Image:
    image = stills.image(path)
    width, height = image.size
    left = int(round(min(max(cam.x, 0.0), width - 2)))
    top = int(round(min(max(cam.y, 0.0), height - 2)))
    right = int(round(min(max(cam.x + cam.w, left + 2), width)))
    bottom = int(round(min(max(cam.y + cam.h, top + 2), height)))
    return image.crop((left, top, right, bottom)).resize(OUT_SIZE, Image.BILINEAR)


# --- overlays -------------------------------------------------------------


def map_thumb(path: Path) -> Image.Image:
    """8-bit-safe thumbnail of an exported map.

    The app writes 16-bit grayscale PNGs for height and curvature, which PIL
    reports as I;16; converting those straight to RGB clips every pixel to 255,
    so the data is normalised by percentile first.
    """
    image = Image.open(path)
    if image.mode.startswith("I"):
        data = np.asarray(image).astype(np.float32)
        if data.max() > 1.0:
            data /= 65535.0
        low, high = np.percentile(data, [0.5, 99.5])
        span = max(float(high) - float(low), 1e-4)
        scaled = np.clip((data - low) / span, 0.0, 1.0)
        return Image.fromarray((scaled * 255.0).astype(np.uint8), "L").convert("RGB")
    return image.convert("RGB")


def draw_cursor(frame: Image.Image, position, pressed: bool = False) -> None:
    draw = ImageDraw.Draw(frame, "RGBA")
    x, y = position
    scale = 1.35
    points = [(0, 0), (0, 20), (5, 15), (9, 22), (12, 20), (8, 13), (14, 12)]
    polygon = [(x + px * scale, y + py * scale) for px, py in points]
    draw.polygon(polygon, outline=(20, 22, 28, 255))
    draw.line(polygon + [polygon[0]], fill=(20, 22, 28, 255), width=4)
    draw.polygon(polygon, fill=(250, 250, 252, 255))
    if pressed:
        draw.ellipse([x - 16, y - 16, x + 16, y + 16], outline=(255, 255, 255, 130), width=3)


def draw_click_pulse(frame: Image.Image, position, t: float) -> None:
    """Expanding ring, 0 at the start of the click and 1 when it fades out."""
    draw = ImageDraw.Draw(frame, "RGBA")
    x, y = position
    radius = 10 + 46 * ease(t)
    alpha = int(190 * (1.0 - ease(t)))
    draw.ellipse([x - radius, y - radius, x + radius, y + radius],
                 outline=(255, 214, 150, alpha), width=5)
    core = 12 + 10 * ease(t)
    draw.ellipse([x - core, y - core, x + core, y + core],
                 fill=(255, 190, 96, int(alpha * 0.35)))


def caption_card(step: int, total: int, title: str, hint: str, alpha: float) -> Image.Image:
    if alpha <= 0.01:
        return
    left, bottom = 56, OUT_SIZE[1] - 60
    box = (left, bottom - 132, left + 900, bottom)
    overlay = Image.new("RGBA", OUT_SIZE, (0, 0, 0, 0))
    paint = ImageDraw.Draw(overlay)
    paint.rounded_rectangle(box, radius=18, fill=(CARD[0], CARD[1], CARD[2], int(232 * alpha)))
    paint.rounded_rectangle(box, radius=18, outline=(CARD_EDGE[0], CARD_EDGE[1], CARD_EDGE[2],
                                                     int(220 * alpha)), width=2)
    badge = (left + 18, bottom - 118, left + 96, bottom - 74)
    paint.rounded_rectangle(badge, radius=12, fill=(int(ACCENT[0]), int(ACCENT[1]),
                                                    int(ACCENT[2]), int(235 * alpha)))
    tint = int(255 * alpha)
    paint.text((badge[0] + 14, badge[1] + 9), f"{step:02d}", font=TITLE_FONT,
               fill=(20, 16, 8, tint))
    paint.text((left + 116, bottom - 118), title, font=TITLE_FONT_BIG, fill=(TEXT[0], TEXT[1],
                                                                             TEXT[2], tint))
    paint.text((left + 116, bottom - 66), hint, font=HINT_FONT, fill=(MUTED[0], MUTED[1],
                                                                      MUTED[2], tint))
    return overlay


def progress_bar(draw: ImageDraw.ImageDraw, step: int, total: int, sub: float) -> None:
    filled = (step - 1 + sub) / max(total - 1, 1)
    left, top = OUT_SIZE[0] - 56 - 320, OUT_SIZE[1] - 46
    draw.rounded_rectangle((left, top, left + 320, top + 6), radius=3, fill=CARD_EDGE)
    draw.rounded_rectangle((left, top, left + int(320 * min(filled, 1.0)), top + 6), radius=3,
                           fill=ACCENT)


TITLE_FONT = None
TITLE_FONT_BIG = None
HINT_FONT = None
CARD_FONT = None


def load_fonts() -> None:
    global TITLE_FONT, TITLE_FONT_BIG, HINT_FONT, CARD_FONT
    TITLE_FONT = font("Inter-SemiBold.ttf", 34)
    TITLE_FONT_BIG = font("Inter-SemiBold.ttf", 40)
    HINT_FONT = font("Inter-Regular.ttf", 26)
    CARD_FONT = font("Inter-Regular.ttf", 22)


# --- shot model -----------------------------------------------------------


@dataclass
class Shot:
    still: Path
    title: str
    hint: str
    sec: float
    focus: tuple | None
    points: list
    size: tuple = (1904, 1041)
    phases: list = field(default_factory=list)

    @property
    def has_action(self) -> bool:
        return bool(self.points)


def build_shot(step: dict, stills: Stills) -> Shot:
    path = Path(step["still"])
    return Shot(
        still=path,
        title=step.get("title", ""),
        hint=step.get("hint", ""),
        sec=float(step.get("sec", 7.0)),
        focus=step.get("focus"),
        points=[tuple(point) for point in step.get("cursor_points", [])],
        size=stills.size(path),
    )


# --- frame renderer -------------------------------------------------------


class Renderer:
    def __init__(self, shots: list[Shot], meta: dict):
        self.stills = Stills()
        self.shots = shots
        self.meta = meta
        self.timeline = self._build_timeline()

    def _build_timeline(self) -> list[dict]:
        """Split every shot into glide / click / dissolve / hold segments."""
        timeline: list[dict] = []
        timeline.append({"kind": "intro", "frames": int(3.6 * FPS)})
        for index, shot in enumerate(self.shots):
            glide = 0.0
            click = 0.0
            dissolve = 0.42 if index > 0 else 0.0
            if shot.has_action and index > 0:
                glide = 0.55
                click = 0.28
            hold = max(1.2, shot.sec - glide - click - dissolve)
            total = glide + click + dissolve + hold
            shot.sec = total
            timeline.append({"kind": "shot", "index": index, "glide": glide, "click": click,
                             "dissolve": dissolve, "hold": hold})
        timeline.append({"kind": "outro", "frames": int(5.0 * FPS)})
        return timeline

    def total_frames(self) -> int:
        count = 0
        for entry in self.timeline:
            if entry["kind"] in ("intro", "outro"):
                count += entry["frames"]
            else:
                count += int(round(sum(entry[key] for key in ("glide", "click", "dissolve",
                                                              "hold")) * FPS))
        return count

    # -- camera helpers ---------------------------------------------------

    def shot_cam(self, index: int, phase: str, t: float) -> Cam:
        shot = self.shots[index]
        full = full_cam(shot.size)
        target = focus_cam(shot.size, shot.focus)
        if phase == "hold":
            # push in, then a slow drift keeps the frame alive
            pushed = full.lerp(target, ease(t / 0.7)) if t < 0.7 else target
            if t >= 0.7:
                drift = (t - 0.7) / max(0.001, 1.0 - 0.7)
                zoom = 1.0 + 0.035 * drift
                centre_x = pushed.x + pushed.w / 2.0
                centre_y = pushed.y + pushed.h / 2.0
                width = min(pushed.w * zoom, shot.size[0])
                height = min(pushed.h * zoom, shot.size[1])
                return Cam(min(max(centre_x - width / 2.0, 0.0), shot.size[0] - width),
                           min(max(centre_y - height / 2.0, 0.0), shot.size[1] - height),
                           width, height)
            return pushed
        if phase == "glide":
            action = shot.focus or points_rect(shot.points)
            return full.lerp(focus_cam(shot.size, action), ease(t) * 0.6)
        return full.lerp(target, 0.0)

    def cursor_path(self, index: int) -> list:
        """Where the pointer travels during this shot's action phase."""
        shot = self.shots[index]
        previous = self.shots[index - 1].points if index > 0 and self.shots[index - 1].points else []
        start = previous[-1] if previous else (shot.size[0] - 30, shot.size[1] - 30)
        if not shot.points:
            return [start, start]
        return [start, *shot.points]

    # -- frame ------------------------------------------------------------

    def frame(self, position: int) -> Image.Image:
        cursor = position
        for entry in self.timeline:
            span = (entry["frames"] if entry["kind"] in ("intro", "outro")
                    else int(round(sum(entry[key] for key in ("glide", "click", "dissolve",
                                                              "hold")) * FPS)))
            if cursor < span:
                if entry["kind"] == "intro":
                    return self._intro_frame(cursor / span)
                if entry["kind"] == "outro":
                    return self._outro_frame(cursor / span)
                return self._shot_frame(entry, cursor / FPS)
            cursor -= span
        raise IndexError(position)

    def _shot_frame(self, entry: dict, time: float) -> Image.Image:
        index = entry["index"]
        shot = self.shots[index]
        glide, click, dissolve, hold = (entry[key] for key in ("glide", "click", "dissolve",
                                                              "hold"))
        previous = self.shots[index - 1] if index > 0 else None
        phase_time = time
        phase = "hold"
        if glide and phase_time < glide:
            phase = "glide"
        elif click and phase_time < glide + click:
            phase = "click"
        elif dissolve and phase_time < glide + click + dissolve:
            phase = "dissolve"
        else:
            phase_time = max(0.0, time - glide - click - dissolve)
            phase = "hold"

        if phase in ("glide", "click"):
            base = previous or shot
            cam = self.shot_cam(index, "glide", phase_time / max(glide, 0.001) if glide else 1.0)
            frame = render_still(self.stills, base.still, cam)
        elif phase == "dissolve":
            t = ease(phase_time / max(dissolve, 0.001))
            old_cam = self.shot_cam(index - 1, "hold", 1.0) if previous else None
            old = render_still(self.stills, previous.still, old_cam) if previous else None
            new = render_still(self.stills, shot.still, full_cam(shot.size).lerp(
                focus_cam(shot.size, shot.focus), 0.15 * t))
            frame = Image.blend(old, new, t) if old else new
        else:
            cam = self.shot_cam(index, "hold", phase_time / max(hold, 0.001))
            frame = render_still(self.stills, shot.still, cam)

        # caption alpha
        alpha = 1.0
        if phase in ("glide", "click"):
            alpha = 0.0
        elif phase == "dissolve":
            alpha = ease(phase_time / max(dissolve, 0.001))
        elif phase_time < 0.35:
            alpha = ease(phase_time / 0.35)

        overlay = caption_card(index + 1, len(self.shots), shot.title, shot.hint, alpha)

        draw = ImageDraw.Draw(frame, "RGBA")
        progress_bar(draw, index + 1, len(self.shots),
                     min(1.0, time / max(shot.sec, 0.001)))
        draw_watermark(frame)

        # caption card, drawn above the chrome
        if overlay:
            frame.paste(overlay, (0, 0), overlay)

        # pointer
        path = self.cursor_path(index)
        if phase == "glide" and glide > 0:
            t = phase_time / glide
            position_xy = walk_path(path, t)
            draw_cursor(frame, position_xy)
        elif phase == "click":
            draw_cursor(frame, path[-1], pressed=True)
            draw_click_pulse(frame, path[-1], phase_time / max(click, 0.001))
        elif phase in ("dissolve", "hold") and shot.has_action:
            # the pointer parks where the action happened, then leaves the frame
            fade = min(1.0, phase_time / 0.9)
            if fade < 1.0:
                draw_cursor(frame, path[-1])
        return frame

    def _intro_frame(self, t: float) -> Image.Image:
        shot = self.shots[0]
        cam = full_cam(shot.size).lerp(focus_cam(shot.size, (400, 120, 1100, 800)), ease(t))
        frame = render_still(self.stills, shot.still, cam)
        overlay = Image.new("RGBA", OUT_SIZE, (0, 0, 0, 0))
        paint = ImageDraw.Draw(overlay)
        paint.rectangle((0, 0, OUT_SIZE[0], OUT_SIZE[1]), fill=(SCRIM[0], SCRIM[1], SCRIM[2], 214))
        appear = ease(min(1.0, t / 0.35))
        paint.text((112, 372), "N-shade", font=LOGO_FONT,
                   fill=(ACCENT[0], ACCENT[1], ACCENT[2], int(255 * appear)))
        paint.text((112, 440), "Designing a Sci-Fi Panel", font=HEAD_FONT,
                   fill=(TEXT[0], TEXT[1], TEXT[2], int(255 * appear)))
        paint.text((116, 540), "step by step in Rust — vectors, bevels, cuts, masks and maps",
                   font=SUB_FONT, fill=(MUTED[0], MUTED[1], MUTED[2], int(255 * appear)))
        rule_y = 520
        paint.rounded_rectangle((112, rule_y, 112 + int(560 * ease(min(1.0, t / 0.6))), rule_y + 5),
                                radius=3, fill=(ACCENT[0], ACCENT[1], ACCENT[2], int(230 * appear)))
        frame.paste(overlay, (0, 0), overlay)
        return frame

    def _outro_frame(self, t: float) -> Image.Image:
        # the 3D preview reads better as a backdrop than the last shot
        hero = next((shot for shot in self.shots if shot.still.stem.endswith("three_d")),
                    self.shots[-1])
        frame = render_still(self.stills, hero.still, focus_cam(hero.size, (500, 150, 900, 780)))
        overlay = Image.new("RGBA", OUT_SIZE, (0, 0, 0, 0))
        paint = ImageDraw.Draw(overlay)
        paint.rectangle((0, 0, OUT_SIZE[0], OUT_SIZE[1]), fill=(SCRIM[0], SCRIM[1], SCRIM[2], 224))
        appear = ease(min(1.0, t / 0.3))
        paint.text((112, 150), "Exported", font=HEAD_FONT,
                   fill=(TEXT[0], TEXT[1], TEXT[2], int(255 * appear)))
        paint.text((116, 224), "normal · height · curvature · project — 16-bit, OpenGL (Y+)",
                   font=SUB_FONT, fill=(MUTED[0], MUTED[1], MUTED[2], int(255 * appear)))
        x = 112
        for name, label in (("normal.png", "Normal"), ("height.png", "Height"),
                            ("curvature.png", "Curvature")):
            path = ROOT / "output" / "tutorial" / "export" / name
            if path.exists():
                thumb = map_thumb(path)
                thumb.thumbnail((300, 300), Image.LANCZOS)
                box_h = thumb.height
                pad = 10
                paint.rounded_rectangle((x - pad, 320 - pad, x + thumb.width + pad,
                                         320 + box_h + pad), radius=10,
                                        fill=(255, 255, 255, int(24 * appear)))
                layer = Image.new("RGBA", thumb.size, (0, 0, 0, 0))
                layer.paste(thumb, (0, 0))
                layer.putalpha(int(255 * appear))
                frame.paste(layer, (x, 320), layer)
                paint.text((x, 320 + box_h + 22), label, font=CAPTION_FONT,
                           fill=(MUTED[0], MUTED[1], MUTED[2], int(255 * appear)))
            x += 360
        paint.text((112, 780), "N-shade — Normal Map Studio", font=LOGO_FONT,
                   fill=(ACCENT[0], ACCENT[1], ACCENT[2], int(255 * appear)))
        paint.text((116, 848), self.meta.get("subtitle", "vector normal maps, live 3D preview"),
                   font=SUB_FONT, fill=(MUTED[0], MUTED[1], MUTED[2], int(255 * appear)))
        frame.paste(overlay, (0, 0), overlay)
        return frame


def walk_path(path: list, t: float) -> tuple:
    """Position along a polyline at eased parameter t."""
    if len(path) < 2:
        return path[0] if path else (0, 0)
    eased = ease(t)
    segments = len(path) - 1
    scaled = eased * segments
    index = min(int(scaled), segments - 1)
    local = scaled - index
    (x0, y0), (x1, y1) = path[index], path[index + 1]
    return (x0 + (x1 - x0) * local, y0 + (y1 - y0) * local)


def draw_watermark(frame: Image.Image) -> None:
    """Small brand pill, kept off the app's own chrome as much as possible."""
    overlay = Image.new("RGBA", OUT_SIZE, (0, 0, 0, 0))
    paint = ImageDraw.Draw(overlay)
    paint.rounded_rectangle((44, 30, 236, 74), radius=10, fill=(10, 11, 14, 150))
    paint.text((60, 40), "N-shade", font=WATERMARK_FONT, fill=ACCENT)
    frame.paste(overlay, (0, 0), overlay)


LOGO_FONT = HEAD_FONT = SUB_FONT = CAPTION_FONT = WATERMARK_FONT = WATERMARK_SUB_FONT = None


def load_extra_fonts() -> None:
    global LOGO_FONT, HEAD_FONT, SUB_FONT, CAPTION_FONT, WATERMARK_FONT, WATERMARK_SUB_FONT
    LOGO_FONT = font("Inter-SemiBold.ttf", 44)
    HEAD_FONT = font("Inter-SemiBold.ttf", 76)
    SUB_FONT = font("Inter-Regular.ttf", 30)
    CAPTION_FONT = font("Inter-Regular.ttf", 24)
    WATERMARK_FONT = font("Inter-SemiBold.ttf", 26)
    WATERMARK_SUB_FONT = font("Inter-Regular.ttf", 20)


def find_ffmpeg() -> Path:
    for candidate in FFMPEG_CANDIDATES:
        if candidate.is_absolute():
            if candidate.exists():
                return candidate
        else:
            from shutil import which
            found = which(str(candidate))
            if found:
                return Path(found)
    raise SystemExit("ffmpeg not found — install it or point FFMPEG_CANDIDATES at a build")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--steps", default="output/tutorial/final/steps.json")
    parser.add_argument("--extra", default=None,
                        help="a second steps.json appended to the same video")
    parser.add_argument("--out", default="output/tutorial/nshade_scifi_panel_tutorial.mp4")
    parser.add_argument("--preview", default="",
                        help="comma separated frame numbers to render as PNG instead of encoding")
    parser.add_argument("--srt-only", action="store_true",
                        help="rewrite the subtitle file from the existing timeline, no encode")
    options = parser.parse_args()

    load_fonts()
    load_extra_fonts()

    root = ROOT
    steps_path = (root / options.steps) if not Path(options.steps).is_absolute() else Path(options.steps)
    payload = json.loads(steps_path.read_text(encoding="utf-8"))
    raw_steps = list(payload["steps"])
    if options.extra:
        extra_path = root / options.extra
        if extra_path.exists():
            raw_steps.extend(json.loads(extra_path.read_text(encoding="utf-8"))["steps"])
    for index, step in enumerate(raw_steps, start=1):
        path = Path(step["still"])
        step["still"] = str(path if path.is_absolute() else root / path)
        step.setdefault("index", index)

    stills = Stills(limit=2)
    shots = [build_shot(step, stills) for step in raw_steps]
    renderer = Renderer(shots, {"subtitle": f"{len(shots)} steps · vector booleans, knife cuts, "
                                           f"masks, live text, 3D preview"})

    total = renderer.total_frames()
    print(f"{len(shots)} shots -> {total} frames ({total / FPS:.1f}s) at {FPS} fps")

    if options.srt_only:
        out_path = root / options.out
        write_srt(root, raw_steps, shots, out_path.with_suffix(".srt"))
        return 0

    if options.preview:
        out_dir = root / "output" / "tutorial" / "preview"
        out_dir.mkdir(parents=True, exist_ok=True)
        picks = sorted({int(value) for value in options.preview.split(",") if value.strip()})
        for pick in picks:
            frame = min(pick, total - 1)
            renderer.frame(frame).save(out_dir / f"frame_{frame:05d}.png")
            print(f"preview frame {frame}")
        return 0

    out_path = root / options.out
    out_path.parent.mkdir(parents=True, exist_ok=True)
    ffmpeg = find_ffmpeg()
    command = [
        str(ffmpeg), "-y", "-loglevel", "error",
        "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", f"{OUT_SIZE[0]}x{OUT_SIZE[1]}",
        "-r", str(FPS), "-i", "-",
        "-an", "-c:v", "libx264", "-preset", "medium", "-crf", "19",
        "-pix_fmt", "yuv420p", "-movflags", "+faststart", str(out_path),
    ]
    writer = subprocess.Popen(command, stdin=subprocess.PIPE)
    assert writer.stdin is not None
    for index in range(total):
        frame = np.asarray(renderer.frame(index).convert("RGB"), dtype=np.uint8)
        writer.stdin.write(frame.tobytes())
        if index % 300 == 0:
            print(f"  {index}/{total} ({100 * index / total:.0f}%)", flush=True)
    writer.stdin.close()
    if writer.wait() != 0:
        print("ffmpeg failed", file=sys.stderr)
        return 1
    print(f"wrote {out_path}")
    write_srt(root, raw_steps, shots, out_path.with_suffix(".srt"))
    renderer.frame(min(total - 40, total - 1)).convert("RGB").save(
        out_path.with_name("tutorial_poster.png"))
    return 0


def write_srt(root: Path, raw_steps: list[dict], shots: list[Shot], path: Path) -> None:
    """Subtitle file with the same timings the video uses."""
    lines = []
    clock = 3.6
    for index, step in enumerate(raw_steps, start=1):
        shot = shots[index - 1]
        start, end = clock, clock + shot.sec
        title = step.get("title", "")
        hint = step.get("hint", "")
        text = f"{title} — {hint}" if hint else title
        lines.append(f"{index}\n{srt_time(start)} --> {srt_time(end)}\n{text}\n")
        clock = end
    path.write_text("\n".join(lines), encoding="utf-8-sig")
    print(f"wrote {path}")


def srt_time(seconds: float) -> str:
    millis = int(round(seconds * 1000))
    hours, rest = divmod(millis, 3_600_000)
    minutes, rest = divmod(rest, 60_000)
    secs, millis = divmod(rest, 1000)
    return f"{hours:02d}:{minutes:02d}:{secs:02d},{millis:03d}"


if __name__ == "__main__":
    sys.exit(main())
