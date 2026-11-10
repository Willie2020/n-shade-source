"""Operators: connect, pair, pull, selection, material, release.

None of these push an undo step.  `bl_options` is deliberately `{"REGISTER"}`
only: connecting a socket is not a file edit, and an undoable operator would
snapshot the document every time the user toggled the bridge.
"""

import os
import tempfile

import bpy

from . import apply, catalog, handlers, material, pairing, prefs, protocol, state
from .client import CLIENT

REGISTER_ONLY = {"REGISTER"}


def _settings(context):
    scene = getattr(context, "scene", None)
    return getattr(scene, "n_shade_bridge", None) if scene is not None else None


class NSHADE_OT_connect(bpy.types.Operator):
    bl_idname = "nshade.connect"
    bl_label = "Connect"
    bl_description = "Connect to the N-shade bridge on the loopback interface"
    bl_options = REGISTER_ONLY

    @classmethod
    def poll(cls, context):
        return not CLIENT.connected

    def execute(self, context):
        prefs_object = prefs.get_prefs()
        if not prefs.value("token", ""):
            ok, message = pairing.apply_to(prefs_object, protocol.PROTOCOL_VERSION)
            if not ok:
                self.report(
                    {"ERROR"},
                    "%s - turn the bridge on in N-shade first" % message,
                )
                return {"CANCELLED"}
            self.report({"INFO"}, message)
        if CLIENT.running:
            self.report({"INFO"}, "The bridge is already connecting")
            return {"CANCELLED"}
        handlers.resume_auto()
        port = int(prefs.value("port", 47100))
        if not CLIENT.start("127.0.0.1", port, prefs.value("token", "")):
            self.report({"ERROR"}, "Could not start the bridge thread")
            return {"CANCELLED"}
        return {"FINISHED"}


class NSHADE_OT_disconnect(bpy.types.Operator):
    bl_idname = "nshade.disconnect"
    bl_label = "Disconnect"
    bl_description = "Close the bridge connection"
    bl_options = REGISTER_ONLY

    @classmethod
    def poll(cls, context):
        return CLIENT.running or CLIENT.connected

    def execute(self, context):
        handlers.suspend_auto()
        handlers.clear_queue()
        CLIENT.stop()
        state.STATE.mark_closed("disconnected")
        return {"FINISHED"}


class NSHADE_OT_autopair(bpy.types.Operator):
    bl_idname = "nshade.autopair"
    bl_label = "Auto-pair"
    bl_description = "Read port and token from the file N-shade writes when its bridge starts"
    bl_options = REGISTER_ONLY

    def execute(self, context):
        ok, message = pairing.apply_to(prefs.get_prefs(), protocol.PROTOCOL_VERSION)
        self.report({"INFO"} if ok else {"ERROR"}, message)
        return {"FINISHED"} if ok else {"CANCELLED"}


class NSHADE_OT_pull(bpy.types.Operator):
    bl_idname = "nshade.pull"
    bl_label = "Pull now"
    bl_description = "Request the active map at the pull size"
    bl_options = REGISTER_ONLY

    @classmethod
    def poll(cls, context):
        return CLIENT.connected

    def execute(self, context):
        map_id = handlers.request_now()
        if map_id is None:
            self.report({"WARNING"}, "Nothing to pull")
            return {"CANCELLED"}
        self.report({"INFO"}, "Requested %s" % catalog.label_for(map_id))
        return {"FINISHED"}


class NSHADE_OT_pull_selected(bpy.types.Operator):
    bl_idname = "nshade.pull_selected"
    bl_label = "Get selected"
    bl_description = "Request every ticked map at the pull size"
    bl_options = REGISTER_ONLY

    @classmethod
    def poll(cls, context):
        return CLIENT.connected

    def execute(self, context):
        maps = handlers.wanted()
        if not maps:
            self.report({"WARNING"}, "Tick at least one map first")
            return {"CANCELLED"}
        count = handlers.request_many(maps)
        self.report({"INFO"}, "Queued %d map(s)" % count)
        return {"FINISHED"}


class NSHADE_OT_pull_all(bpy.types.Operator):
    bl_idname = "nshade.pull_all"
    bl_label = "Get all maps"
    bl_description = "Request every map the host can serve at the pull size"
    bl_options = REGISTER_ONLY

    @classmethod
    def poll(cls, context):
        return CLIENT.connected

    def execute(self, context):
        ready = [info.id for info in state.STATE.catalog if info.ready]
        if not ready:
            ready = [info.id for info in state.STATE.catalog]
        count = handlers.request_many(ready)
        self.report({"INFO"}, "Queued %d map(s)" % count)
        return {"FINISHED"}


class NSHADE_OT_toggle_map(bpy.types.Operator):
    bl_idname = "nshade.toggle_map"
    bl_label = "Toggle map"
    bl_description = "Include or exclude this map from Get selected"
    bl_options = REGISTER_ONLY

    map_id: bpy.props.StringProperty(name="Map", default="")

    def execute(self, context):
        handlers.toggle_wanted(self.map_id)
        return {"FINISHED"}


class NSHADE_OT_select_maps(bpy.types.Operator):
    bl_idname = "nshade.select_maps"
    bl_label = "Select maps"
    bl_description = "Tick every map in the catalogue, or clear the selection"
    bl_options = REGISTER_ONLY

    mode: bpy.props.EnumProperty(
        name="Mode",
        items=(("ALL", "All", "Tick every map"), ("NONE", "None", "Untick all")),
        default="ALL",
    )

    def execute(self, context):
        if self.mode == "ALL":
            handlers.set_wanted([info.id for info in state.STATE.catalog])
        else:
            handlers.set_wanted([])
        return {"FINISHED"}


class NSHADE_OT_blender_preset(bpy.types.Operator):
    bl_idname = "nshade.blender_preset"
    bl_label = "Blender defaults"
    bl_description = (
        "Live at 1024, pulls at 2048 with 16-bit height, and the material "
        "options Blender wants"
    )
    bl_options = REGISTER_ONLY

    def execute(self, context):
        data = _settings(context)
        if data is None:
            self.report({"ERROR"}, "Scene settings unavailable")
            return {"CANCELLED"}
        data.live = True
        data.live_map = "normal"
        data.live_size = "1024"
        data.pull_size = "2048"
        data.bits16 = True
        data.strength = 1.0
        data.use_orm = True
        data.use_ao = True
        data.bump_from_height = True
        data.auto_wire = True
        data.auto_assign = True
        handlers.set_wanted(["normal", "height", "curvature", "edge_wear"])
        self.report(
            {"INFO"},
            "Live 1024, pull 2048, 16-bit height; normal/height/curvature/edge wear ticked",
        )
        return {"FINISHED"}


class NSHADE_OT_set_stream_map(bpy.types.Operator):
    bl_idname = "nshade.set_stream_map"
    bl_label = "Stream this map"
    bl_description = "Follow this map with the realtime stream"
    bl_options = REGISTER_ONLY

    map_id: bpy.props.StringProperty(name="Map", default="")

    def execute(self, context):
        data = _settings(context)
        if data is None or not self.map_id:
            return {"CANCELLED"}
        data.live_map = self.map_id
        handlers.request_now(self.map_id, force=True)
        return {"FINISHED"}


class NSHADE_OT_update_material(bpy.types.Operator):
    bl_idname = "nshade.update_material"
    bl_label = "Update material"
    bl_description = "Wire every streamed map into the live material"
    bl_options = REGISTER_ONLY

    @classmethod
    def poll(cls, context):
        return apply.has_any()

    def execute(self, context):
        result = handlers.refresh_material()
        if result is None:
            self.report({"WARNING"}, "No streamed maps to wire yet")
            return {"CANCELLED"}
        self.report({"INFO"}, "Wired into %s" % result.name)
        return {"FINISHED"}


class NSHADE_OT_push_mesh(bpy.types.Operator):
    bl_idname = "nshade.push_mesh"
    bl_label = "Send mesh to N-shade"
    bl_description = (
        "Export the selected mesh (its UV layout, normals and vertex positions) and "
        "hand it to N-shade, so the 2D canvas and the UV overlay match what you are "
        "texturing here"
    )
    bl_options = REGISTER_ONLY

    @classmethod
    def poll(cls, context):
        return CLIENT.connected and bool(_mesh_selection(context))

    def execute(self, context):
        objects = _mesh_selection(context)
        if not objects:
            self.report({"WARNING"}, "Select a mesh first")
            return {"CANCELLED"}

        # One fixed scratch file: repeated sends overwrite it, so the add-on
        # never accumulates meshes on disk.
        directory = os.path.join(tempfile.gettempdir(), "n-shade")
        try:
            os.makedirs(directory, exist_ok=True)
        except OSError as error:
            self.report({"ERROR"}, "Could not create %s: %s" % (directory, error))
            return {"CANCELLED"}
        path = os.path.join(directory, "blender_mesh.obj")

        view_layer = context.view_layer
        previous = [obj for obj in view_layer.objects if obj.select_get()]
        active = view_layer.objects.active
        try:
            for obj in previous:
                obj.select_set(False)
            for obj in objects:
                obj.select_set(True)
            view_layer.objects.active = objects[0]
            bpy.ops.wm.obj_export(
                filepath=path,
                export_selected_objects=True,
                apply_modifiers=True,
                export_uv=True,
                export_normals=True,
                export_materials=False,
                export_triangulated_mesh=False,
                # The OBJ convention the N-shade importer expects.
                forward_axis="NEGATIVE_Z",
                up_axis="Y",
            )
        except Exception as error:  # an operator reports, it does not raise
            self.report({"ERROR"}, "Could not export the mesh: %s" % error)
            return {"CANCELLED"}
        finally:
            for obj in objects:
                obj.select_set(False)
            for obj in previous:
                obj.select_set(True)
            view_layer.objects.active = active

        name = objects[0].name if len(objects) == 1 else "%d objects" % len(objects)
        if CLIENT.push_mesh(path, name) is None:
            self.report({"ERROR"}, "The bridge is not connected")
            return {"CANCELLED"}
        self.report({"INFO"}, "Sent %s to N-shade" % name)
        return {"FINISHED"}


def _mesh_selection(context):
    """Selected mesh objects, or the active one when nothing is selected."""
    view_layer = getattr(context, "view_layer", None)
    if view_layer is None:
        return []
    selected = [
        obj
        for obj in view_layer.objects
        if obj.type == "MESH" and obj.select_get() and obj.visible_get()
    ]
    if selected:
        return selected
    active = view_layer.objects.active
    if active is not None and active.type == "MESH":
        return [active]
    return []


class NSHADE_OT_release(bpy.types.Operator):
    bl_idname = "nshade.release"
    bl_label = "Release images"
    bl_description = "Delete the streamed map images and their GPU buffers"
    bl_options = REGISTER_ONLY

    def execute(self, context):
        apply.purge()
        handlers.unpin()
        handlers.reset_wire_cache()
        data = _settings(context)
        name = str(getattr(data, "material", "") or material.DEFAULT_MATERIAL)
        target = bpy.data.materials.get(name)
        if target is not None:
            material.clear(target)
        state.STATE.maps = {}
        state.STATE.frames = 0
        state.STATE.bytes = 0
        self.report({"INFO"}, "Streamed images removed")
        return {"FINISHED"}
