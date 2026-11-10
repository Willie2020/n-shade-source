"""N-shade Bridge - live maps from the N-shade editor, inside Blender.

Install the `n_shade_bridge` folder as an extension or add-on (see
`blender/README.md`), open 3D Viewport > Sidebar (N) > N-shade, and the panel
connects on its own when N-shade has its bridge switched on.

The add-on owns no state that matters: it opens a loopback socket, asks the
host for maps, and writes the answers into reusable image datablocks.  Anything
the host does not send is simply not updated.
"""

bl_info = {
    "name": "N-shade Bridge",
    "author": "N-shade",
    "version": (2, 1, 0),
    "blender": (4, 3, 0),
    "location": "3D Viewport > Sidebar > N-shade",
    "description": "Stream normal, height and PBR maps live from N-shade",
    "category": "Material",
}

import atexit

import bpy

from . import client, compat, handlers, ops, prefs, props, ui

CLASSES = (
    prefs.BridgePreferences,
    props.BridgeSceneSettings,
    ops.NSHADE_OT_connect,
    ops.NSHADE_OT_disconnect,
    ops.NSHADE_OT_autopair,
    ops.NSHADE_OT_pull,
    ops.NSHADE_OT_pull_selected,
    ops.NSHADE_OT_pull_all,
    ops.NSHADE_OT_toggle_map,
    ops.NSHADE_OT_select_maps,
    ops.NSHADE_OT_blender_preset,
    ops.NSHADE_OT_set_stream_map,
    ops.NSHADE_OT_update_material,
    ops.NSHADE_OT_push_mesh,
    ops.NSHADE_OT_release,
    ui.NSHADE_MT_stream_map,
    ui.NSHADE_PT_bridge,
    ui.NSHADE_PT_maps,
    ui.NSHADE_PT_material,
)


def register():
    for cls in CLASSES:
        bpy.utils.register_class(cls)
    bpy.types.Scene.n_shade_bridge = bpy.props.PointerProperty(
        type=props.BridgeSceneSettings
    )
    handlers.start()
    atexit.register(_at_exit)
    if not compat.supported():
        print(
            "N-shade bridge: Blender %s is older than 3.6; this addon targets 4.3 "
            "and newer, so expect missing features." % compat.version_string()
        )


def unregister():
    atexit.unregister(_at_exit)
    try:
        handlers.shutdown()
    except Exception as error:  # never let teardown raise inside Blender
        print("N-shade bridge: shutdown failed:", error)
    if getattr(bpy.types.Scene, "n_shade_bridge", None) is not None:
        del bpy.types.Scene.n_shade_bridge
    for cls in reversed(CLASSES):
        bpy.utils.unregister_class(cls)


def _at_exit():
    # Sockets and threads only: by the time this runs, bpy is gone.
    try:
        client.CLIENT.stop()
    except Exception:
        pass
