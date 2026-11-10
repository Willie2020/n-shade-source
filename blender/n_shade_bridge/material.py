"""Node graph built from the streamed maps.

The goal is a material that looks the same in Blender as the map stack does in
N-shade, using only nodes that have been stable since 3.4:

    Normal          Image -> Normal Map -> Principled.Normal
    Base Colour     Image (sRGB) -> Principled.Base Color
    ORM             Image -> Separate Color -> Roughness / Metallic, R -> AO
    Rough/Metallic  separate images -> the two scalar inputs
    AO              Image -> Separate Color -> multiply into base colour
    Height          Image -> Bump -> Principled.Normal (only when no normal map)

Conflicts are handled by ownership rather than by clearing the material:

* every node this module creates is named with the ``NS `` prefix;
* each sync removes links that touch one of those nodes, then removes the
  nodes that are no longer needed, so a map that disappears (or an ORM map
  replaced by separate roughness/metallic maps) cannot leave a stale texture
  or a second link behind;
* user nodes survive, and a user link into a shaded Principled input is left in
  place; only links that touch an ``NS `` node are ever added or removed, and an
  existing node is never moved;
* a dedicated material is used by default. Assigning it to an object swaps the
  active slot's material reference, leaving the previous datablock and its node
  tree untouched; Blender's normal orphan rules apply if nothing else uses it.

Everything is addressed by an internal node name (``NS Normal Tex``), so the
function is idempotent: pressing "Update material" a hundred times leaves one
node of each kind, one link per socket, and never stacks duplicates.  Sockets
are looked up by name with a fallback to identifier/index because both differ
between Blender versions and localisations.
"""

import bpy

from . import compat

DEFAULT_MATERIAL = "N-Shade Live"
# Node names in this namespace belong to the add-on. Image datablocks use
# `NS_`; keeping the two prefixes distinct makes a node and its image easy to
# tell apart in Blender's outliner.
PREFIX = "NS "

NORMAL_TEX = PREFIX + "Normal Tex"
NORMAL_MAP = PREFIX + "Normal Map"
BASE_TEX = PREFIX + "Base Colour Tex"
ORM_TEX = PREFIX + "ORM Tex"
ORM_SPLIT = PREFIX + "ORM Split"
AO_TEX = PREFIX + "AO Tex"
AO_SPLIT = PREFIX + "AO Split"
AO_MIX = PREFIX + "AO Mix"
ROUGH_TEX = PREFIX + "Roughness Tex"
METAL_TEX = PREFIX + "Metallic Tex"
HEIGHT_TEX = PREFIX + "Height Tex"
BUMP = PREFIX + "Bump"

# Left-to-right columns: images, converters, BSDF, output. Rows are separated
# by 160 units so the generated tree is readable without moving existing nodes.
POS_NORMAL_TEX = (-760.0, 340.0)
POS_NORMAL_MAP = (-460.0, 340.0)
POS_BASE_TEX = (-760.0, 100.0)
POS_AO_TEX = (-760.0, -60.0)
POS_AO_SPLIT = (-520.0, -60.0)
POS_AO_MIX = (-260.0, 100.0)
POS_ORM_TEX = (-760.0, -220.0)
POS_ORM_SPLIT = (-460.0, -220.0)
POS_ROUGH_TEX = (-760.0, -380.0)
POS_METAL_TEX = (-760.0, -540.0)
POS_HEIGHT_TEX = (-760.0, -700.0)
POS_BUMP = (-280.0, -700.0)
POS_BSDF = (0.0, 100.0)
POS_OUTPUT = (340.0, 100.0)

# Which streamed map feeds which role, best first.
_CANDIDATES = {
    "normal": ("normal", "pbr_normal"),
    "base_color": ("pbr_base_color",),
    "orm": ("pbr_orm",),
    "roughness": ("pbr_roughness",),
    "metallic": ("pbr_metallic",),
    "ao": ("pbr_ao",),
    "height": ("height", "pbr_height", "pbr_displacement"),
}


def pick(images):
    """Resolve the available maps into one image per role."""
    roles = {}
    for role, candidates in _CANDIDATES.items():
        for map_id in candidates:
            image = images.get(map_id)
            if image is not None:
                roles[role] = (map_id, image)
                break
    return roles


def signature(images, strength=1.0, use_orm=True, use_ao=True, bump_from_height=True):
    """A cheap key for the graph `sync` would build.

    Pixel updates keep the key stable: images are resized in place and node
    links stay valid, so there is no reason to rebuild the tree for every
    streamed frame.  A datablock replacement (the 8/16-bit switch) changes the
    id and therefore the key, so the next frame re-points the texture node.
    """
    roles = pick(images)
    return (
        tuple(
            sorted(
                (role, map_id, id(image))
                for role, (map_id, image) in roles.items()
            )
        ),
        round(float(strength), 4),
        bool(use_orm),
        bool(use_ao),
        bool(bump_from_height),
    )


def ensure_material(name=None):
    """The live material, created on first use.

    An existing material with the requested name is reused deliberately: the
    panel's Material field is the explicit choice of datablock.  Only nodes in
    the ``NS `` namespace are ever edited, so the user's own nodes and links in
    that material survive an update.
    """
    name = name or DEFAULT_MATERIAL
    material = bpy.data.materials.get(name)
    if material is None:
        material = bpy.data.materials.new(name)
    material.use_nodes = True
    tree = material.node_tree
    if tree is None:
        raise RuntimeError("material %s has no node tree" % material.name)
    return material


def sync(material, images, strength=1.0, use_orm=True, use_ao=True, bump_from_height=True):
    """Rebuild the live node graph for `images`.

    Returns the list of roles that were wired, in the order they were applied,
    so the panel can report what actually reached the material.
    """
    roles = pick(images)
    material.use_nodes = True
    tree = material.node_tree
    if tree is None:
        raise RuntimeError("material %s has no node tree" % material.name)
    links = tree.links

    bsdf = compat.principled_node(tree)
    if bsdf is None:
        bsdf = tree.nodes.new("ShaderNodeBsdfPrincipled")
        bsdf.location = POS_BSDF
    output = compat.output_node(tree)
    if output is None:
        output = tree.nodes.new("ShaderNodeOutputMaterial")
        output.location = POS_OUTPUT
    surface = compat.socket(output, "Surface")
    shader = compat.socket(bsdf, "BSDF", "output")
    if surface is not None and shader is not None and not _occupied(links, surface):
        links.new(shader, surface)

    # Our nodes are a closed set.  Breaking their links first means a role that
    # vanished cannot keep feeding the Principled, and the rebuild cannot stack
    # a second link on a socket.
    _clear_managed_links(tree)
    keep = set()
    applied = []

    normal_in = compat.socket(bsdf, "Normal")
    normal_present = False

    # --- normal -----------------------------------------------------------
    if "normal" in roles:
        map_id, image = roles["normal"]
        tex = _image_node(
            tree, NORMAL_TEX, image, "N-shade %s" % map_id, POS_NORMAL_TEX, keep
        )
        normal = compat.node(
            tree, NORMAL_MAP, "ShaderNodeNormalMap", "N-shade normal", POS_NORMAL_MAP
        )
        keep.add(normal.name)
        if hasattr(normal, "space"):
            normal.space = "TANGENT"
        strength_socket = compat.socket(normal, "Strength")
        if strength_socket is not None:
            strength_socket.default_value = float(strength)
        color_out = compat.socket(tex, "Color", "output")
        color_in = compat.socket(normal, "Color")
        if _relink(links, color_out, color_in):
            map_out = compat.socket(normal, "Normal", "output")
            if _relink(links, map_out, normal_in):
                normal_present = True
                applied.append("normal")

    # --- packed ORM or separate scalars -----------------------------------
    ao_out = None
    if use_orm and "orm" in roles:
        _, orm_image = roles["orm"]
        orm = _image_node(tree, ORM_TEX, orm_image, "N-shade ORM", POS_ORM_TEX, keep)
        split = compat.node(
            tree, ORM_SPLIT, "ShaderNodeSeparateColor", "N-shade ORM split", POS_ORM_SPLIT
        )
        keep.add(split.name)
        if hasattr(split, "mode"):
            split.mode = "RGB"
        orm_out = compat.socket(orm, "Color", "output")
        split_in = compat.socket(split, "Color")
        if _relink(links, orm_out, split_in):
            red = compat.socket(split, "Red", "output")
            green = compat.socket(split, "Green", "output")
            blue = compat.socket(split, "Blue", "output")
            if use_ao:
                ao_out = red
            if _wire_scalar(links, bsdf, ("Roughness",), green):
                applied.append("roughness")
            if _wire_scalar(links, bsdf, ("Metallic",), blue):
                applied.append("metallic")
    else:
        # Maps stream in one at a time; ORM may not have arrived yet.  The
        # separate roughness/metallic images are a complete fallback even when
        # Packed ORM is on, and the packed map takes over on the next sync.
        if "roughness" in roles:
            _, image = roles["roughness"]
            tex = _image_node(
                tree, ROUGH_TEX, image, "N-shade roughness", POS_ROUGH_TEX, keep
            )
            split = _value_output(tree, tex, keep)
            if _wire_scalar(links, bsdf, ("Roughness",), split):
                applied.append("roughness")
        if "metallic" in roles:
            _, image = roles["metallic"]
            tex = _image_node(
                tree, METAL_TEX, image, "N-shade metallic", POS_METAL_TEX, keep
            )
            split = _value_output(tree, tex, keep)
            if _wire_scalar(links, bsdf, ("Metallic",), split):
                applied.append("metallic")

    # --- standalone AO ----------------------------------------------------
    # The host can serve `pbr_ao` independently of the packed ORM texture.
    if use_ao and ao_out is None and "ao" in roles and "base_color" in roles:
        _, image = roles["ao"]
        tex = _image_node(tree, AO_TEX, image, "N-shade AO", POS_AO_TEX, keep)
        split = compat.node(
            tree, AO_SPLIT, "ShaderNodeSeparateColor", "N-shade AO split", POS_AO_SPLIT
        )
        keep.add(split.name)
        if hasattr(split, "mode"):
            split.mode = "RGB"
        color_out = compat.socket(tex, "Color", "output")
        split_in = compat.socket(split, "Color")
        if _relink(links, color_out, split_in):
            ao_out = compat.socket(split, "Red", "output")

    # --- base colour, with the glTF-style AO multiply ---------------------
    base_in = compat.socket(bsdf, "Base Color") or compat.socket(bsdf, "Base Colour")
    if "base_color" in roles:
        map_id, image = roles["base_color"]
        tex = _image_node(
            tree, BASE_TEX, image, "N-shade %s" % map_id, POS_BASE_TEX, keep
        )
        base_out = compat.socket(tex, "Color", "output")
        wired = False
        if base_out is not None and base_in is not None:
            if use_ao and ao_out is not None and not _occupied(links, base_in):
                mix = compat.node(
                    tree, AO_MIX, "ShaderNodeMix", "N-shade AO", POS_AO_MIX
                )
                keep.add(mix.name)
                if hasattr(mix, "data_type"):
                    mix.data_type = "RGBA"
                if hasattr(mix, "blend_type"):
                    mix.blend_type = "MULTIPLY"
                factor = _mix_socket(mix, "Factor", "Float")
                if factor is not None:
                    factor.default_value = 1.0
                a = _mix_socket(mix, "A", "Color")
                b = _mix_socket(mix, "B", "Color")
                _relink(links, base_out, a)
                _relink(links, ao_out, b)
                result = _mix_socket(mix, "Result", "Color", direction="output")
                if _relink(links, result, base_in):
                    wired = True
                    applied.append("ao")
            else:
                wired = _relink(links, base_out, base_in)
        if wired:
            applied.append("base_color")

    # --- bump from height -------------------------------------------------
    if bump_from_height and not normal_present and "height" in roles:
        map_id, image = roles["height"]
        tex = _image_node(
            tree, HEIGHT_TEX, image, "N-shade %s" % map_id, POS_HEIGHT_TEX, keep
        )
        bump = compat.node(tree, BUMP, "ShaderNodeBump", "N-shade bump", POS_BUMP)
        keep.add(bump.name)
        height_in = compat.socket(bump, "Height")
        height_out = _value_output(tree, tex, keep)
        if _relink(links, height_out, height_in):
            strength_socket = compat.socket(bump, "Strength")
            if strength_socket is not None:
                strength_socket.default_value = float(strength)
            bump_out = compat.socket(bump, "Normal", "output")
            if _relink(links, bump_out, normal_in):
                applied.append("bump")

    # Anything left from an earlier map set is no longer referenced.
    _prune(tree, keep)
    return applied


def clear(material):
    """Remove every `NS ` node from `material`, leaving user nodes alone.

    Used by Release images: the streamed image datablocks are gone, so keeping
    texture nodes that point at nothing would be a stale, confusing graph.
    """
    tree = getattr(material, "node_tree", None)
    if tree is None:
        return 0
    _clear_managed_links(tree)
    removed = len([node for node in tree.nodes if _managed(node)])
    _prune(tree, set())
    return removed


def apply_to_object(obj, material):
    """Give `obj` the live material, returning False when it cannot hold one.

    Only the active slot's material *reference* is swapped.  The previous
    datablock keeps its node tree and stays available in the file; Blender's
    normal orphan rules apply if nothing else uses it.
    """
    if obj is None or obj.type != "MESH" or obj.data is None:
        return False
    slots = obj.material_slots
    if not slots:
        obj.data.materials.append(material)
        return True
    index = getattr(obj, "active_material_index", 0)
    if index < 0 or index >= len(slots):
        index = 0
    if slots[index].material == material:
        return True
    slots[index].material = material
    return True


def active_mesh(context):
    view_layer = getattr(context, "view_layer", None)
    obj = view_layer.objects.active if view_layer is not None else None
    return obj if obj is not None and obj.type == "MESH" else None


# ---------------------------------------------------------------------------
# helpers
# ---------------------------------------------------------------------------


def _managed(node):
    return str(getattr(node, "name", "")).startswith(PREFIX)


def _clear_managed_links(tree):
    """Drop every link that touches an N-shade node, in either direction."""
    for link in list(tree.links):
        source = getattr(link.from_socket, "node", None)
        target = getattr(link.to_socket, "node", None)
        if _managed(source) or _managed(target):
            tree.links.remove(link)


def _prune(tree, keep):
    """Remove N-shade nodes this sync did not rebuild."""
    for node in list(tree.nodes):
        if _managed(node) and node.name not in keep:
            tree.nodes.remove(node)


def _image_node(tree, name, image, label, location, keep):
    tex = compat.node(tree, name, "ShaderNodeTexImage", label, location)
    keep.add(tex.name)
    tex.label = label
    tex.image = image
    tex.interpolation = "Linear"
    tex.extension = "EXTEND"
    return tex


def _value_output(tree, tex, keep):
    """A scalar taken from a colour image: any channel will do.

    The tree is passed in rather than read from `node.id_data`, which returns
    the owning ID (the material) for a node and has changed meaning across
    versions before.
    """
    separate = compat.node(
        tree,
        tex.name + " Value",
        "ShaderNodeSeparateColor",
        "",
        (tex.location[0] + 240.0, tex.location[1]),
    )
    keep.add(separate.name)
    if hasattr(separate, "mode"):
        separate.mode = "RGB"
    color_out = compat.socket(tex, "Color", "output")
    color_in = compat.socket(separate, "Color")
    if color_out is not None and color_in is not None:
        _relink(tree.links, color_out, color_in)
    return compat.socket(separate, "Green", "output") or compat.socket(
        separate, "Red", "output"
    )


def _wire_scalar(links, bsdf, names, from_socket):
    if from_socket is None:
        return False
    for name in names:
        target = compat.socket(bsdf, name)
        if target is not None:
            return _relink(links, from_socket, target)
    return False


def _occupied(links, socket):
    """True when a non-managed link already feeds `socket`."""
    for link in links:
        if link.to_socket == socket:
            source = getattr(link.from_socket, "node", None)
            return not _managed(source)
    return False


def _mix_socket(node, name, kind, direction="input"):
    """ShaderNodeMix sockets carry identifiers like A_Color / Result_Color."""
    identifier = "%s_%s" % (name, kind)
    found = compat.socket_by_identifier(node, identifier, direction)
    if found is not None:
        return found
    collection = node.inputs if direction == "input" else node.outputs
    for item in collection:
        if item.name == name and getattr(item, "type", "") in ("RGBA", "VALUE"):
            return item
    return None


def _relink(links, from_socket, to_socket):
    """Replace an existing input link instead of stacking duplicates.

    A link from outside the `NS ` namespace belongs to the user and is left in
    place: the add-on reports the role as unwired rather than silently cutting
    a connection the user made.  Our own links were already removed by
    `_clear_managed_links`, so a rebuild never stacks two links on a socket.
    Returns False when either socket is missing or the input is occupied by a
    user link.
    """
    if from_socket is None or to_socket is None:
        return False
    for link in list(links):
        if link.to_socket == to_socket:
            source = getattr(link.from_socket, "node", None)
            if not _managed(source):
                return False
            links.remove(link)
    links.new(from_socket, to_socket)
    return True
