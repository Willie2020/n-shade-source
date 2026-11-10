#!/usr/bin/env python3
"""Drives N-shade through a scripted step list and captures one still per step.

The app window is forced topmost at the full screen and the real cursor is
moved by the script, exactly like tools/capture_window.ps1, but the process
stays alive for the whole run so every still belongs to one continuous session.
Stills are captured from the screen DC (GDI BitBlt) because PrintWindow returns
black for a wgpu surface.

Usage:
    python tools/tutorial_record.py --plan discover
    python tools/tutorial_record.py --plan final

Output: output/tutorial/<plan>/step_XX.png plus steps.json with the metadata the
composer needs (cursor path, click points, focus rectangle, captions).
"""

from __future__ import annotations

import argparse
import ctypes
import json
import subprocess
import sys
import time
from pathlib import Path

import numpy as np
from PIL import Image

user32 = ctypes.windll.user32
gdi32 = ctypes.windll.gdi32

ROOT = Path(__file__).resolve().parent.parent

# --- win32 plumbing -------------------------------------------------------

SWP_NOSIZE = 0x0001
SWP_NOZORDER = 0x0004
SWP_SHOWWINDOW = 0x0040
HWND_TOPMOST = ctypes.c_void_p(-1)
SRCCOPY = 0x00CC0020
CAPTUREBLT = 0x40000000

MOUSEEVENTF_MOVE = 0x0001
MOUSEEVENTF_LEFTDOWN = 0x0002
MOUSEEVENTF_LEFTUP = 0x0004
MOUSEEVENTF_RIGHTDOWN = 0x0008
MOUSEEVENTF_RIGHTUP = 0x0010
MOUSEEVENTF_WHEEL = 0x0800

KEYEVENTF_EXTENDEDKEY = 0x0001
KEYEVENTF_KEYUP = 0x0002


class RECT(ctypes.Structure):
    _fields_ = [("left", ctypes.c_long), ("top", ctypes.c_long),
                ("right", ctypes.c_long), ("bottom", ctypes.c_long)]


class POINT(ctypes.Structure):
    _fields_ = [("x", ctypes.c_long), ("y", ctypes.c_long)]


ENUM_WINDOWS_PROC = ctypes.WINFUNCTYPE(ctypes.c_bool, ctypes.c_void_p, ctypes.c_void_p)


def find_window_for_pid(pid: int, title_part: str = "N-shade"):
    """Return the first visible window owned by *pid* whose title matches."""
    found = []

    def callback(hwnd, _lparam):
        if not user32.IsWindowVisible(hwnd):
            return True
        length = user32.GetWindowTextLengthW(hwnd)
        if length == 0:
            return True
        buffer = ctypes.create_unicode_buffer(length + 1)
        user32.GetWindowTextW(hwnd, buffer, length + 1)
        window_pid = ctypes.c_ulong()
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(window_pid))
        if window_pid.value == pid and title_part in buffer.value:
            found.append(hwnd)
            return False
        return True

    user32.EnumWindows(ENUM_WINDOWS_PROC(callback), 0)
    return found[0] if found else None


# --- key tables -----------------------------------------------------------

VK = {
    "enter": 0x0D, "return": 0x0D, "escape": 0x1B, "esc": 0x1B, "delete": 0x2E,
    "backspace": 0x08, "space": 0x20, "tab": 0x09, "home": 0x24, "end": 0x23,
    "left": 0x25, "up": 0x26, "right": 0x27, "down": 0x28,
    "f2": 0x71, "minus": 0xBD, "comma": 0xBC, "period": 0xBE,
    "0": 0x30, "1": 0x31, "2": 0x32, "3": 0x33, "4": 0x34,
    "5": 0x35, "6": 0x36, "7": 0x37, "8": 0x38, "9": 0x39,
}
for _letter in "ABCDEFGHIJKLMNOPQRSTUVWXYZ":
    VK[_letter.lower()] = ord(_letter)

MODS = {"ctrl": 0x11, "shift": 0x10, "alt": 0x12}


class Recorder:
    def __init__(self, exe: Path, out_dir: Path, args: list[str], settle: float):
        self.exe = exe
        self.out_dir = out_dir
        self.args = args
        self.settle = settle
        self.hwnd = None
        self.origin = (0, 0)
        self.size = (0, 0)
        self.cursor = (0.0, 0.0)
        self.process = None

    # --- window -----------------------------------------------------------

    def launch(self) -> None:
        self.process = subprocess.Popen([str(self.exe), *self.args], cwd=str(ROOT))
        deadline = time.time() + 60.0
        while time.time() < deadline:
            time.sleep(0.5)
            hwnd = find_window_for_pid(self.process.pid)
            if hwnd:
                self.hwnd = hwnd
                break
        else:
            raise RuntimeError("no N-shade window appeared")
        # Restore, size and pin topmost so the screen capture sees the full
        # client area, then give the GPU pipeline a moment for the first frames.
        user32.ShowWindow(self.hwnd, 9)  # SW_RESTORE
        user32.SetWindowPos(self.hwnd, None, 0, 0, 1920, 1080,
                            SWP_NOSIZE | SWP_SHOWWINDOW)
        user32.SetWindowPos(self.hwnd, HWND_TOPMOST, 0, 0, 1920, 1080, SWP_SHOWWINDOW)
        user32.SetForegroundWindow(self.hwnd)
        time.sleep(4.0)
        self._update_geometry()

    def _update_geometry(self) -> None:
        rect = RECT()
        user32.GetClientRect(self.hwnd, ctypes.byref(rect))
        point = POINT(0, 0)
        user32.ClientToScreen(self.hwnd, ctypes.byref(point))
        self.origin = (point.x, point.y)
        self.size = (rect.right - rect.left, rect.bottom - rect.top)

    def close(self) -> None:
        if self.process and self.process.poll() is None:
            user32.PostMessageW(self.hwnd, 0x0010, 0, 0)  # WM_CLOSE
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.process.kill()

    # --- input ------------------------------------------------------------

    def move(self, x: int, y: int, speed: float = 0.012) -> None:
        """Glide the real cursor; the app sees every intermediate position."""
        sx, sy = self.to_screen(x, y)
        steps = max(1, int((abs(sx - self.cursor[0]) + abs(sy - self.cursor[1])) / 24))
        for index in range(1, steps + 1):
            t = index / steps
            # ease-in-out so the pointer accelerates and settles like a hand
            ease = t * t * (3 - 2 * t)
            px = self.cursor[0] + (sx - self.cursor[0]) * ease
            py = self.cursor[1] + (sy - self.cursor[1]) * ease
            user32.SetCursorPos(int(px), int(py))
            self.cursor = (px, py)
            time.sleep(speed)
        user32.SetCursorPos(sx, sy)
        self.cursor = (float(sx), float(sy))
        time.sleep(0.08)

    def to_screen(self, x: int, y: int) -> tuple[int, int]:
        return self.origin[0] + int(x), self.origin[1] + int(y)

    def _key(self, code: int, up: bool = False) -> None:
        flags = KEYEVENTF_KEYUP if up else 0
        if code in (0x25, 0x26, 0x27, 0x28, 0x24, 0x23):
            flags |= KEYEVENTF_EXTENDEDKEY
        user32.keybd_event(code, 0, flags, 0)

    def press(self, name: str) -> None:
        """Press one shortcut, e.g. "r" or "ctrl+shift+z"."""
        lowered = name.lower()
        mods = [MODS[part] for part in MODS if lowered.startswith(f"{part}+")]
        # support chained modifiers: ctrl+shift+z
        tokens = lowered.split("+")
        mods = [MODS[token] for token in tokens[:-1] if token in MODS]
        key = tokens[-1]
        if key not in VK:
            raise ValueError(f"unknown key {name!r}")
        for mod in mods:
            self._key(mod)
        time.sleep(0.04)
        self._key(VK[key])
        time.sleep(0.06)
        self._key(VK[key], up=True)
        for mod in reversed(mods):
            self._key(mod, up=True)
        time.sleep(0.2)

    def type_text(self, text: str) -> None:
        for char in text:
            if char == " ":
                self.press("space")
                continue
            lowered = char.lower()
            if lowered not in VK and char not in "-":
                continue
            if char == "-":
                self.press("minus")
                continue
            if char.isupper():
                self._key(MODS["shift"])
                time.sleep(0.03)
            self.press(lowered)
            if char.isupper():
                self._key(MODS["shift"], up=True)

    def click(self, x: int, y: int, double: bool = False, mod: str | None = None) -> None:
        self.move(x, y)
        if mod:
            self._key(MODS[mod])
            time.sleep(0.05)
        for _ in range(2 if double else 1):
            user32.mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, 0)
            time.sleep(0.05)
            user32.mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0)
            time.sleep(0.12 if double else 0.45)
        if mod:
            self._key(MODS[mod], up=True)
            time.sleep(0.2)

    def right_click(self, x: int, y: int) -> None:
        self.move(x, y)
        user32.mouse_event(MOUSEEVENTF_RIGHTDOWN, 0, 0, 0, 0)
        time.sleep(0.05)
        user32.mouse_event(MOUSEEVENTF_RIGHTUP, 0, 0, 0, 0)
        time.sleep(0.55)

    def drag(self, start: tuple[int, int], end: tuple[int, int], mod: str | None = None,
             samples: int = 34) -> None:
        x1, y1 = start
        x2, y2 = end
        sx1, sy1 = self.to_screen(x1, y1)
        sx2, sy2 = self.to_screen(x2, y2)
        self.move(x1, y1)
        if mod:
            self._key(MODS[mod])
            time.sleep(0.06)
        user32.mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, 0)
        time.sleep(0.08)
        for index in range(1, samples + 1):
            # Linear sampling: an eased drag would make the app see uneven
            # pointer deltas and wobble the drawn shape.
            t = index / samples
            x, y = sx1 + (sx2 - sx1) * t, sy1 + (sy2 - sy1) * t
            user32.SetCursorPos(int(x), int(y))
            self.cursor = (x, y)
            time.sleep(0.018)
        time.sleep(0.18)
        user32.mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0)
        if mod:
            time.sleep(0.06)
            self._key(MODS[mod], up=True)
        time.sleep(0.4)

    def wheel(self, x: int, y: int, delta: int) -> None:
        self.move(x, y)
        user32.mouse_event(MOUSEEVENTF_WHEEL, 0, 0, delta & 0xFFFFFFFF, 0)
        time.sleep(0.4)

    # --- capture ----------------------------------------------------------

    def shoot(self, path: Path) -> Path:
        width, height = self.size
        screen = user32.GetDC(0)
        memory = gdi32.CreateCompatibleDC(screen)
        bitmap = gdi32.CreateCompatibleBitmap(screen, width, height)
        gdi32.SelectObject(memory, bitmap)
        gdi32.BitBlt(memory, 0, 0, width, height, screen,
                     self.origin[0], self.origin[1], SRCCOPY | CAPTUREBLT)

        class BIHDR(ctypes.Structure):
            _fields_ = [("biSize", ctypes.c_uint32), ("biWidth", ctypes.c_long),
                        ("biHeight", ctypes.c_long), ("biPlanes", ctypes.c_uint16),
                        ("biBitCount", ctypes.c_uint16), ("biCompression", ctypes.c_uint32),
                        ("biSizeImage", ctypes.c_uint32), ("biXPelsPerMeter", ctypes.c_long),
                        ("biYPelsPerMeter", ctypes.c_long), ("biClrUsed", ctypes.c_uint32),
                        ("biClrImportant", ctypes.c_uint32)]

        header = BIHDR(40, width, -height, 1, 32, 0, 0, 0, 0, 0, 0)
        buffer = (ctypes.c_char * (width * height * 4))()
        gdi32.GetDIBits(memory, bitmap, 0, height, buffer, ctypes.byref(header), 0)
        array = np.frombuffer(buffer, dtype=np.uint8).reshape(height, width, 4)
        Image.fromarray(array[:, :, [2, 1, 0]]).save(path)

        gdi32.DeleteDC(memory)
        user32.ReleaseDC(0, screen)
        gdi32.DeleteObject(bitmap)
        return path

    def park_cursor(self) -> None:
        """Hover the status bar corner so no widget keeps a hover highlight."""
        self.move(max(4, self.size[0] - 6), self.size[1] - 6)

    # --- step execution ---------------------------------------------------

    def run(self, steps: list[dict], tag: str) -> list[dict]:
        self.out_dir.mkdir(parents=True, exist_ok=True)
        recorded = []
        for index, step in enumerate(steps, start=1):
            if self.process.poll() is not None:
                raise RuntimeError(f"app exited at step {index}")
            name = step.get("id", f"{index:02d}")
            still = self.out_dir / f"{tag}_{index:02d}_{name}.png"
            points, clicks = [], []
            for action in step.get("actions", []):
                kind = action[0]
                if kind == "sleep":
                    time.sleep(action[1])
                elif kind == "key":
                    self.press(action[1])
                elif kind == "type":
                    self.type_text(action[1])
                elif kind == "click":
                    self.click(action[1][0], action[1][1],
                               mod=action[2] if len(action) > 2 else None)
                    points.append(action[1])
                    clicks.append(action[1])
                elif kind == "double":
                    self.click(action[1][0], action[1][1], double=True)
                    points.append(action[1])
                    clicks.append(action[1])
                elif kind == "rclick":
                    self.right_click(*action[1])
                    points.append(action[1])
                    clicks.append(action[1])
                elif kind == "drag":
                    self.drag(*action[1:3], mod=action[3] if len(action) > 3 else None)
                    points.extend([action[1], action[2]])
                    clicks.append(action[2])
                elif kind == "wheel":
                    self.wheel(action[1], action[2], action[3])
                    points.append((action[1], action[2]))
                else:
                    raise ValueError(f"unknown action {kind!r}")
            time.sleep(step.get("settle", self.settle))
            if step.get("park", True):
                self.park_cursor()
                time.sleep(0.45)
            self.shoot(still)
            entry = dict(step)
            entry["still"] = str(still)
            entry["index"] = index
            entry["cursor_points"] = points
            entry["clicks"] = clicks
            entry.pop("actions", None)
            recorded.append(entry)
            print(f"[{index:02d}] {name} -> {still.name}", flush=True)
        return recorded


# --- step plans -----------------------------------------------------------

# Discovery plan: learn the real on-screen coordinates of the widgets the final
# recording needs. Nothing here has to be pretty, it only has to be readable.
DISCOVER = [
    {"id": "boot", "actions": []},
    {"id": "rect_drawn", "actions": [("key", "r"), ("drag", (700, 330), (1230, 700))]},
    {"id": "props_shape", "actions": [("click", (150, 210))]},
    {"id": "second_rect", "actions": [("key", "r"), ("drag", (790, 400), (980, 520))]},
    {"id": "row_menu", "actions": [("rclick", (150, 210))]},
    {"id": "export_open", "actions": [("click", (442, 819))]},
    {"id": "tab_mesh", "actions": [("key", "escape"), ("click", (1614, 94))]},
    {"id": "tab_pbr", "actions": [("click", (1853, 94))]},
    {"id": "tab_view", "actions": [("click", (717, 105))]},
]

DISCOVER2 = [
    {"id": "boot", "actions": []},
    {"id": "plate", "actions": [("key", "r"), ("drag", (700, 330), (1230, 700))]},
    {"id": "props_scroll", "actions": [
        ("click", (250, 225)), ("wheel", 1750, 620, -720), ("wheel", 1750, 620, -720),
    ]},
    {"id": "props_more", "actions": [("wheel", 1750, 620, -720), ("wheel", 1750, 620, -720)]},
    {"id": "export_open", "actions": [("click", (447, 908))]},
    {"id": "add_menu", "actions": [("click", (439, 105))]},
    {"id": "knife_draw", "actions": [
        ("key", "escape"), ("key", "p"), ("click", (660, 520)), ("click", (1330, 520)),
        ("key", "enter"),
    ]},
    {"id": "knife_props", "actions": [("click", (250, 300)), ("wheel", 1750, 620, -720)]},
    {"id": "cut_menu", "actions": [("rclick", (250, 300))]},
    {"id": "after_cut", "actions": [("key", "escape"), ("click", (231, 595))]},
    {"id": "split", "actions": [("click", (712, 100))]},
    {"id": "three_d", "actions": [("click", (549, 100))]},
    {"id": "mesh_tab", "actions": [("click", (1627, 91))]},
    {"id": "tools_menu", "actions": [("click", (531, 34))]},
]

DISCOVER3 = [
    {"id": "boot", "actions": []},
    {"id": "plate", "actions": [("key", "r"), ("drag", (700, 330), (1230, 700))]},
    {"id": "inner", "actions": [("key", "r"), ("drag", (790, 400), (1150, 560))]},
    {"id": "multi", "actions": [("key", "v"), ("click", (250, 225)),
                                ("click", (250, 300), "ctrl")]},
    {"id": "bool_menu", "actions": [("click", (439, 105)), ("click", (455, 621))]},
    {"id": "bool_pick", "actions": [("click", (640, 640))]},
    {"id": "group_props", "actions": [
        ("key", "escape"), ("key", "ctrl+g"), ("click", (1548, 91)),
        ("wheel", 1750, 600, 720), ("wheel", 1750, 600, 720),
    ]},
    {"id": "text", "actions": [
        ("key", "t"), ("click", (995, 300)), ("type", "NX-07"), ("key", "enter"),
    ]},
    {"id": "view_dropdown", "actions": [("click", (975, 100))]},
    {"id": "height_pick", "actions": [("click", (975, 150))]},
    {"id": "mask_paint", "actions": [
        ("click", (250, 225)), ("click", (1737, 779)), ("key", "b"),
        ("drag", (800, 450), (1100, 600)),
    ]},
    {"id": "three_d", "actions": [("click", (549, 100)), ("drag", (995, 540), (1160, 470))]},
    {"id": "export_expand", "actions": [
        ("click", (631, 100)), ("click", (1548, 91)), ("click", (447, 908)),
    ]},
]

DISCOVER4 = [
    {"id": "boot", "actions": []},
    {"id": "plate", "actions": [("key", "r"), ("drag", (700, 330), (1230, 700))]},
    {"id": "inner", "actions": [("key", "r"), ("drag", (790, 400), (1150, 560))]},
    {"id": "sel2", "actions": [("key", "v"), ("click", (250, 225)),
                               ("click", (250, 300), "ctrl")]},
    {"id": "menu", "actions": [("click", (439, 105)), ("sleep", 0.6)]},
    {"id": "bool", "actions": [("click", (455, 621)), ("sleep", 0.6)]},
    {"id": "sub", "actions": [("click", (620, 600))]},
]

DISCOVER5 = [
    {"id": "boot", "actions": []},
    {"id": "plate", "actions": [("key", "r"), ("drag", (700, 330), (1230, 700))]},
    {"id": "inner", "actions": [("key", "r"), ("drag", (790, 400), (1150, 560))]},
    {"id": "sel2", "actions": [("key", "v"), ("click", (250, 225)),
                               ("click", (250, 300), "ctrl")]},
    {"id": "menu", "actions": [("click", (439, 105)), ("sleep", 0.5)]},
    {"id": "submenu", "actions": [("click", (430, 582)), ("sleep", 0.6)]},
    {"id": "subtract", "actions": [("click", (640, 621))]},
]

# Final recording: one continuous design session, in story order. Every step
# carries the caption text and the focus rectangle the composer zooms into.
FINAL = [
    {"id": "boot", "title": "A blank document", "hint": "1024 × 1024 · vector layers, live normals",
     "sec": 5.0, "actions": []},
    {"id": "plate", "title": "Draw the base plate", "hint": "Press R, then drag on the 2D canvas",
     "sec": 7.0, "focus": (620, 250, 760, 540),
     "actions": [("key", "r"), ("drag", (700, 330), (1230, 700))]},
    {"id": "shape_props", "title": "One bevel per shape", "hint":
     "Corner · Radius · Depth · Softness · Smooth, all in Properties",
     "sec": 7.5, "focus": (1520, 590, 400, 450),
     "actions": [("click", (250, 225)), ("wheel", 1750, 620, -720), ("wheel", 1750, 620, -720)]},
    {"id": "depth", "title": "Slope and height", "hint": "Radius sets the bevel width · Depth sets the raise",
     "sec": 8.0, "focus": (1545, 740, 390, 140),
     "actions": [("drag", (1817, 781), (1700, 781)), ("drag", (1720, 838), (1795, 838))]},
    {"id": "smooth", "title": "Soften the silhouette", "hint": "Smooth (px) rounds jagged vector edges",
     "sec": 6.5, "focus": (1545, 900, 390, 120),
     "actions": [("drag", (1630, 972), (1700, 972))]},
    {"id": "inner", "title": "Stack a second panel", "hint": "Layers stay vector — nothing is baked to pixels",
     "sec": 7.0, "focus": (660, 300, 700, 460),
     "actions": [("key", "r"), ("drag", (790, 400), (1150, 560))]},
    {"id": "knife", "title": "Draw a cut line", "hint": "Pen: click two points, Enter ends the path open",
     "sec": 8.0, "focus": (600, 400, 800, 260),
     "actions": [("key", "p"), ("click", (660, 520)), ("click", (1330, 520)), ("key", "enter")]},
    {"id": "cut_menu", "title": "Cut shapes below", "hint": "The stroke band is subtracted from everything under it",
     "sec": 6.0, "focus": (110, 170, 440, 580),
     "actions": [("rclick", (250, 225))]},
    {"id": "cut_run", "title": "Every piece stays editable", "hint": "New edges inherit the bevel of the shape they cut",
     "sec": 7.5, "focus": (620, 250, 760, 540),
     "actions": [("click", (310, 594))]},
    {"id": "bool_menu", "title": "Vector booleans", "hint": "Ctrl-click two shapes → Layers → + → Boolean",
     "sec": 8.0, "focus": (110, 90, 540, 640),
     "actions": [("key", "escape"), ("click", (700, 400)), ("click", (970, 470), "ctrl"),
                 ("click", (439, 105)), ("click", (430, 582))]},
    {"id": "bool_apply", "title": "Subtract", "hint": "Outlines combine before rasterizing — no seam, no double bevel",
     "sec": 7.5, "focus": (620, 250, 760, 540),
     "actions": [("click", (628, 676))]},
    {"id": "duplicate", "title": "Copy, paste, nudge", "hint": "Ctrl+C · Ctrl+V · Shift+arrow moves 10 px",
     "sec": 8.0, "focus": (620, 250, 760, 560),
     "actions": [("key", "escape"), ("click", (700, 650)), ("key", "ctrl+c"), ("key", "ctrl+v"),
                 ("key", "shift+down"), ("key", "shift+down"), ("key", "shift+down"),
                 ("key", "shift+down")]},
    {"id": "text", "title": "Live text", "hint": "T, click, type — glyphs are a shape source",
     "sec": 8.0, "focus": (660, 160, 780, 300),
     "actions": [("key", "t"), ("click", (995, 250)), ("key", "ctrl+a"),
                 ("type", "NX-07"), ("key", "enter")]},
    {"id": "mask_paint", "title": "Paint a mask", "hint": "Masks are per layer and follow the shape underneath",
     "sec": 8.5, "focus": (660, 340, 780, 460),
     "actions": [("key", "v"), ("click", (250, 225)),
                 ("wheel", 1750, 600, 720), ("wheel", 1750, 600, 720),
                 ("wheel", 1750, 600, 720), ("wheel", 1750, 600, 720),
                 ("click", (1737, 779)), ("key", "b"), ("drag", (700, 400), (1210, 660))]},
    {"id": "mask_done", "title": "Wear, not erasure", "hint": "Overlay · Mask · Result · Done",
     "sec": 6.0, "focus": (1080, 128, 400, 70),
     "actions": [("click", (1440, 153))]},
    {"id": "height_view", "title": "Height view", "hint": "One dropdown switches Normal · Height · Mask",
     "sec": 7.0, "focus": (580, 180, 840, 760),
     "actions": [("click", (975, 100)), ("click", (975, 186))]},
    {"id": "split", "title": "Split view", "hint": "Author the map, watch the mesh at the same time",
     "sec": 6.5, "actions": [("click", (712, 100))]},
    {"id": "three_d", "title": "3D preview", "hint": "Drag the divider, then orbit — the mesh is live",
     "sec": 7.5, "focus": (520, 160, 940, 820),
     "actions": [("drag", (992, 540), (1420, 540)), ("sleep", 0.4),
                 ("drag", (700, 560), (880, 470))]},
    {"id": "mesh_look", "title": "Mesh, material, light", "hint": "Studio lighting and a camera you can dial in",
     "sec": 7.5, "focus": (1500, 680, 420, 360),
     "actions": [("click", (1627, 91)), ("drag", (1727, 841), (1660, 841)),
                 ("drag", (1660, 917), (1615, 917))]},
    {"id": "export_expand", "title": "The export card", "hint": "Maps, bit depth, Y convention, height source",
     "sec": 7.0, "focus": (80, 550, 440, 490),
     "actions": [("click", (1548, 91)), ("click", (447, 908))]},
    {"id": "export_chips", "title": "Tick the maps you need", "hint":
     "Normal · Height · Curvature · Edge wear · ID · 16-bit",
     "sec": 7.0, "focus": (80, 570, 440, 120),
     "actions": [("click", (400, 607))]},
    {"id": "export_run", "title": "Written to disk", "hint": "16-bit PNGs plus the project file",
     "sec": 7.0, "focus": (80, 900, 440, 140),
     "actions": [("click", (305, 945))]},
]

# Second session: pack the exported normal map into a full material set.
PBR = [
    {"id": "pbr_load", "title": "One map in, a material set out", "hint":
     "The PBR tab reads the exported normal map",
     "sec": 8.5, "focus": (1490, 120, 430, 920), "actions": []},
    {"id": "pbr_maps", "title": "Every channel, generated", "hint":
     "Height · displacement · AO · cavity · curvature · wear · roughness · metallic",
     "sec": 9.0, "focus": (1490, 420, 430, 620),
     "actions": [("click", (1512, 379)), ("wheel", 1750, 600, -720),
                 ("wheel", 1750, 600, -720), ("wheel", 1750, 600, -720)]},
]

# Tail patch: the same document reopened from the exported project, so only the
# view and export shots are re-recorded with the corrected 3D tab target.
PATCH = [
    step for step in FINAL if step["id"] in
    {"height_view", "split", "three_d", "mesh_look", "export_expand", "export_chips",
     "export_run"}
]

PLANS = {
    "discover": (DISCOVER, ["--empty", "--view", "2d"]),
    "discover2": (DISCOVER2, ["--empty", "--view", "2d"]),
    "discover3": (DISCOVER3, ["--empty", "--view", "2d"]),
    "discover4": (DISCOVER4, ["--empty", "--view", "2d"]),
    "discover5": (DISCOVER5, ["--empty", "--view", "2d"]),
    "final": (FINAL, ["--empty", "--view", "2d", "--export-dir", "output/tutorial/export"]),
    "patch": (PATCH, ["--view", "2d", "--project", "output/tutorial/export/project.nsproj",
                      "--export-dir", "output/tutorial/export"]),
    "pbr": (PBR, ["--empty", "--view", "3d", "--pbr", "output/tutorial/export/normal.png",
                  "--pbr-auto"]),
}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--plan",
                        choices=["discover", "discover2", "discover3", "discover4", "discover5",
                                 "final", "patch", "pbr"],
                        default="discover")
    parser.add_argument("--exe", default="target/release/n-shade.exe")
    parser.add_argument("--extra", nargs="*", default=[])
    parser.add_argument("--settle", type=float, default=1.6)
    options = parser.parse_args()

    root = Path(__file__).resolve().parent.parent
    exe = (root / options.exe).resolve()
    if not exe.exists():
        print(f"missing {exe}; run cargo build --release -p ns_app first", file=sys.stderr)
        return 1
    out_dir = root / "output" / "tutorial" / options.plan
    steps, base_args = PLANS[options.plan]
    args = [*base_args, *options.extra]
    recorder = Recorder(exe, out_dir, args, options.settle)
    recorder.launch()
    try:
        recorded = recorder.run(steps, options.plan)
    finally:
        recorder.close()
    payload = {
        "client": {"width": recorder.size[0], "height": recorder.size[1]},
        "steps": recorded,
    }
    (out_dir / "steps.json").write_text(json.dumps(payload, indent=2), encoding="utf-8")
    print(f"wrote {out_dir / 'steps.json'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
