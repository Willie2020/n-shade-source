"""Sidebar panels.

Three stacked panels under the "N-shade" category: connection and streaming,
the map list, and the material.  Drawing only reads plain attributes from
`state.STATE`, so it stays cheap enough to run on every redraw and never blocks
on the socket.
"""

import bpy

from . import apply, catalog, compat, handlers, prefs, state
from .client import CLIENT

CATEGORY = "N-shade"


def _settings(context):
    scene = getattr(context, "scene", None)
    return getattr(scene, "n_shade_bridge", None) if scene is not None else None


def _status_line():
    snapshot = state.STATE
    if CLIENT.connected:
        return "Connected on port %d" % snapshot.port
    if snapshot.phase == "connecting":
        return "Connecting on port %d..." % snapshot.port
    if snapshot.phase == "error":
        return snapshot.last_error or "Connection failed"
    return "Not connected"


def _active_label():
    current = handlers.active_map()
    return catalog.label_for(current)


class NSHADE_MT_stream_map(bpy.types.Menu):
    bl_idname = "NSHADE_MT_stream_map"
    bl_label = "Streaming map"

    def draw(self, context):
        layout = self.layout
        current = handlers.active_map()
        entries = [info for info in state.STATE.catalog if info.ready]
        if not entries:
            layout.label(text="No maps available yet")
            return
        for info in entries:
            row = layout.operator(
                "nshade.set_stream_map",
                text=info.label,
                icon="RADIOBUT_ON" if info.id == current else "RADIOBUT_OFF",
            )
            row.map_id = info.id


class NSHADE_PT_bridge(bpy.types.Panel):
    bl_idname = "NSHADE_PT_bridge"
    bl_label = "N-shade"
    bl_space_type = "VIEW_3D"
    bl_region_type = "UI"
    bl_category = CATEGORY

    def draw(self, context):
        layout = self.layout
        snapshot = state.STATE
        connected = CLIENT.connected

        box = layout.box()
        row = box.row()
        row.label(
            text=_status_line(), icon="CHECKMARK" if connected else "UNLINKED"
        )
        if not CLIENT.connected:
            box.operator("nshade.autopair", icon="FILE_REFRESH")

        if connected:
            box.label(text="Host: N-shade %s" % (snapshot.app_version or "unknown"))
            if snapshot.max_size:
                box.label(text="Host cap: %d px" % snapshot.max_size)
            # `layout.label()` returns None in Blender, so a tooltip cannot be
            # chained onto it; the explanation is a second, smaller line.
            memory = apply.memory_bytes()
            if memory:
                maps = len(apply.bound_maps())
                box.label(
                    text="In Blender: %.1f MB (%d map%s)"
                    % (memory / (1024.0 * 1024.0), maps, "" if maps == 1 else "s")
                )
                if prefs.value("strip_on_save", True):
                    box.label(text="saves stay small", icon="INFO")
            if snapshot.frames:
                box.label(
                    text="%d frame(s) sent, %.1f MB over the socket"
                    % (snapshot.frames, snapshot.bytes / (1024.0 * 1024.0))
                )
            if snapshot.latency_ms:
                box.label(text="Round trip: %.0f ms" % snapshot.latency_ms)
            if snapshot.notice:
                box.label(text=snapshot.notice[:64], icon="INFO")
            if snapshot.last_error:
                box.label(text=snapshot.last_error[:64], icon="ERROR")

        row = layout.row(align=True)
        if connected:
            row.operator("nshade.disconnect", icon="UNLINKED")
        else:
            row.operator("nshade.connect", icon="LINKED")
        row.operator("nshade.pull", text="Pull now", icon="FILE_REFRESH")

        settings = _settings(context)
        if settings is None:
            return

        layout.label(text="Realtime", icon="PLAY")
        column = layout.column(align=True)
        column.prop(settings, "live", text="Stream while editing")
        row = layout.row(align=True)
        row.enabled = connected and settings.live
        row.menu("NSHADE_MT_stream_map", text=_active_label(), icon="IMAGE_DATA")
        row.prop(settings, "live_size", text="")

        if not connected:
            layout.label(text="Turn the bridge on in N-shade Settings", icon="INFO")
        elif not settings.live:
            layout.label(text="Manual mode: pull, or send from N-shade", icon="HAND")

        layout.separator()
        layout.label(text="Mesh", icon="MESH_DATA")
        row = layout.row(align=True)
        row.enabled = connected
        row.operator("nshade.push_mesh", icon="EXPORT")
        layout.label(text="UVs and shape of the selected mesh", icon="INFO")
        layout.operator("nshade.blender_preset", icon="BLENDER")


class NSHADE_PT_maps(bpy.types.Panel):
    bl_idname = "NSHADE_PT_maps"
    bl_label = "N-shade Maps"
    bl_space_type = "VIEW_3D"
    bl_region_type = "UI"
    bl_category = CATEGORY
    bl_options = {"DEFAULT_CLOSED"}

    def draw(self, context):
        layout = self.layout
        settings = _settings(context)
        if settings is None:
            return
        snapshot = state.STATE
        selected = set(catalog.split_selection(settings.wanted))

        row = layout.row(align=True)
        row.prop(settings, "pull_size", text="")
        row.prop(settings, "bits16", text="16-bit")

        available = [info for info in snapshot.catalog]
        if not available:
            layout.label(text="No catalogue yet", icon="INFO")
            return

        group = ""
        column = layout.column(align=True)
        for info in available:
            if info.group != group:
                group = info.group
                column.label(text=catalog.group_label(group))
            line = column.row(align=True)
            ticked = info.id in selected
            toggle = line.operator(
                "nshade.toggle_map",
                text="",
                icon="CHECKBOX_HLT" if ticked else "CHECKBOX_DEHLT",
                emboss=False,
            )
            toggle.map_id = info.id
            label = info.label
            if not info.ready:
                label = "%s (not generated)" % label
            text = line.operator(
                "nshade.toggle_map",
                text=label,
                emboss=False,
                icon="IMAGE_DATA" if snapshot.delivered(info.id) else "NONE",
            )
            text.map_id = info.id
            if snapshot.delivered(info.id):
                line.label(text=apply.describe(info.id))

        layout.separator()
        row = layout.row(align=True)
        row.enabled = CLIENT.connected
        row.operator("nshade.pull_selected", icon="IMPORT")
        row.operator("nshade.pull_all", icon="PACKAGE")
        row = layout.row(align=True)
        row.enabled = CLIENT.connected
        select_all = row.operator("nshade.select_maps", text="All")
        select_all.mode = "ALL"
        select_none = row.operator("nshade.select_maps", text="None")
        select_none.mode = "NONE"

        if snapshot.dropped:
            layout.label(text="Skipped %d stale frame(s)" % snapshot.dropped, icon="INFO")


class NSHADE_PT_material(bpy.types.Panel):
    bl_idname = "NSHADE_PT_material"
    bl_label = "N-shade Material"
    bl_space_type = "VIEW_3D"
    bl_region_type = "UI"
    bl_category = CATEGORY
    bl_options = {"DEFAULT_CLOSED"}

    def draw(self, context):
        layout = self.layout
        settings = _settings(context)
        if settings is None:
            return

        layout.prop(settings, "material", text="")
        layout.prop(settings, "strength")
        column = layout.column(align=True)
        column.prop(settings, "use_orm")
        column.prop(settings, "use_ao")
        column.prop(settings, "bump_from_height")
        layout.prop(settings, "auto_wire")
        layout.prop(settings, "auto_assign")

        row = layout.row(align=True)
        row.operator("nshade.update_material", icon="NODETREE")
        row.operator("nshade.release", text="", icon="TRASH")

        if not apply.has_any():
            layout.label(text="No streamed images yet", icon="IMAGE_DATA")
        if not compat.recommended():
            layout.label(
                text="Blender %s: 4.3 or newer expected" % compat.version_string(),
                icon="ERROR",
            )
