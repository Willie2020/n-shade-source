"""Per-file settings.

These live on the Scene, so each .blend can ask for a different map, size and
material without changing the add-on preferences that every file shares.
"""

import bpy

# Longest side requested from the host. "0" means native, which the host still
# caps at 4096.
SIZE_ITEMS = (
    ("512", "512 px", "Tiny transfer, softest preview"),
    ("1024", "1024 px", "4 MiB per frame at 8 bit: the recommended live size"),
    ("2048", "2048 px", "16 MiB per frame: sharp enough for look-dev"),
    ("4096", "4096 px", "64 MiB per frame: final quality, slowest"),
    ("0", "Native", "Ask for the document resolution (capped at 4096)"),
)


class BridgeSceneSettings(bpy.types.PropertyGroup):
    live: bpy.props.BoolProperty(
        name="Realtime streaming",
        description="Keep pulling the active map while connected. Off: maps "
        "only arrive when you pull them or when N-shade pushes them",
        default=True,
    )
    live_map: bpy.props.StringProperty(
        name="Streaming map",
        description="Which map the realtime stream follows",
        default="normal",
    )
    live_size: bpy.props.EnumProperty(
        name="Live size",
        description="Longest side requested for the streamed map. Smaller "
        "frames update faster and cost less memory",
        items=SIZE_ITEMS,
        default="1024",
    )
    pull_size: bpy.props.EnumProperty(
        name="Pull size",
        description="Longest side requested by Pull, Get selected and pushes "
        "from the app",
        items=SIZE_ITEMS,
        default="2048",
    )
    bits16: bpy.props.BoolProperty(
        name="16-bit height",
        description="Transfer height and displacement as 16-bit samples. "
        "Normals and masks stay 8-bit: their precision cannot be seen and "
        "they would cost twice the memory",
        default=True,
    )
    wanted: bpy.props.StringProperty(
        name="Selected maps",
        description="Map ids ticked in the Maps panel, comma separated",
        default="normal,height",
    )
    material: bpy.props.StringProperty(
        name="Material",
        description="Material datablock the live maps are wired into",
        default="N-Shade Live",
    )
    strength: bpy.props.FloatProperty(
        name="Strength",
        description="Normal Map (and Bump) strength used when wiring",
        default=1.0,
        min=0.0,
        max=2.0,
        soft_max=1.0,
    )
    auto_wire: bpy.props.BoolProperty(
        name="Update material when maps change",
        description="Wire the node graph automatically as maps arrive or the "
        "material options change. Off: press Update material yourself",
        default=True,
    )
    auto_assign: bpy.props.BoolProperty(
        name="Assign to active mesh",
        description="Put the live material in the active object's active slot. "
        "The previous material datablock is left untouched",
        default=True,
    )
    use_orm: bpy.props.BoolProperty(
        name="Packed ORM",
        description="Drive Roughness and Metallic from the packed ORM texture "
        "(R=AO, G=roughness, B=metallic), the layout the glTF exporter and "
        "Blender's own importer use",
        default=True,
    )
    use_ao: bpy.props.BoolProperty(
        name="Multiply AO into base colour",
        description="Blender has no ambient-occlusion input on the Principled "
        "BSDF; the glTF importer multiplies it into base colour, and so does "
        "this",
        default=True,
    )
    bump_from_height: bpy.props.BoolProperty(
        name="Bump from height when there is no normal map",
        description="Feed the height field into a Bump node so a height-only "
        "session still shows surface detail",
        default=True,
    )
