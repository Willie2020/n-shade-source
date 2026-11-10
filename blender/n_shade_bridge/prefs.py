"""Add-on preferences and their safe accessor.

`get_prefs()` must never raise.  It is called from the timer, from operators
and from panel drawing, and in background mode (or during add-on reload) the
add-on entry can be missing from `context.preferences.addons`.  Every caller
therefore goes through `value()`, which falls back to the registered default
instead of exploding mid-frame.
"""

import bpy

PACKAGE = __package__ or "n_shade_bridge"

DEFAULTS = {
    "port": 47100,
    "token": "",
    "auto_connect": True,
    "pause_during_render": True,
    "auto_reconnect": True,
    "strip_on_save": True,
    "max_fps": 8,
    "log_frames": False,
}


class BridgePreferences(bpy.types.AddonPreferences):
    bl_idname = PACKAGE

    port: bpy.props.IntProperty(
        name="Port",
        description="Loopback port printed by N-shade when the bridge starts",
        default=DEFAULTS["port"],
        min=1,
        max=65535,
    )
    token: bpy.props.StringProperty(
        name="Token",
        description="Session token from the host's bridge.json; blocks other "
        "local programs from reading the stream. Auto-pair fills this in",
        default="",
    )
    auto_connect: bpy.props.BoolProperty(
        name="Connect on startup",
        description="Read the pairing file and connect as soon as Blender "
        "starts, so no clicks are needed",
        default=DEFAULTS["auto_connect"],
    )
    pause_during_render: bpy.props.BoolProperty(
        name="Pause while rendering",
        description="Skip updates while a render or bake owns the depsgraph",
        default=DEFAULTS["pause_during_render"],
    )
    auto_reconnect: bpy.props.BoolProperty(
        name="Reconnect automatically",
        description="Retry the loopback connection when the host restarts",
        default=DEFAULTS["auto_reconnect"],
    )
    strip_on_save: bpy.props.BoolProperty(
        name="Keep saves small",
        description="Streamed maps are real image datablocks, so Blender would "
        "write every one of them into the .blend (and again into its backup). "
        "This drops their pixels to a single texel just before the save and "
        "asks N-shade for them again right after, so the file stays small and "
        "the viewport is back a moment later",
        default=DEFAULTS["strip_on_save"],
    )
    max_fps: bpy.props.IntProperty(
        name="Max frames per second",
        description="Upper bound on how often a map is refreshed while "
        "streaming. Cut it down on a heavy scene; the host also refuses to "
        "re-evaluate faster than about sixteen times a second",
        default=DEFAULTS["max_fps"],
        min=1,
        max=60,
    )
    log_frames: bpy.props.BoolProperty(
        name="Log every frame",
        description="Print transfer statistics to the system console; for "
        "diagnosing a slow link",
        default=DEFAULTS["log_frames"],
    )

    def draw(self, context):
        layout = self.layout
        layout.label(text="Connection", icon="LINKED")
        row = layout.row(align=True)
        row.prop(self, "port")
        row.prop(self, "token", text="", icon="LOCKED")
        layout.operator("nshade.autopair", icon="FILE_REFRESH")
        layout.prop(self, "auto_connect")
        layout.separator()
        layout.label(text="Safety", icon="PREFERENCES")
        layout.prop(self, "pause_during_render")
        layout.prop(self, "auto_reconnect")
        layout.separator()
        layout.label(text="Memory", icon="IMAGE_DATA")
        layout.prop(self, "strip_on_save")
        layout.prop(self, "max_fps")
        layout.prop(self, "log_frames")


def get_prefs():
    """The add-on preferences, or None when they are not registered yet."""
    context = getattr(bpy, "context", None)
    preferences = getattr(context, "preferences", None)
    if preferences is None:
        return None
    try:
        addon = preferences.addons.get(PACKAGE)
    except (AttributeError, TypeError):
        return None
    if addon is None:
        return None
    return addon.preferences


def value(name, fallback=None):
    if fallback is None:
        fallback = DEFAULTS.get(name)
    prefs = get_prefs()
    if prefs is None:
        return fallback
    try:
        return getattr(prefs, name)
    except AttributeError:
        return fallback
