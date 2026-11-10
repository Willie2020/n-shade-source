"""Small, boring compatibility layer.

Everything Blender-version specific lives here, and every helper is written
against API that has existed unchanged since 2.8x so the addon runs on 3.6
LTS and on whatever is current.  Where a version could plausibly remove
something, the helper probes for the attribute instead of trusting a version
number: a feature test cannot go stale the way a table of version ranges can.

Blender 4.3 through 5.x is the tested range.  The differences that matter are
node and socket *names* (localised, and renamed wholesale in 4.0) plus the
colour-management vocabulary, all of which are handled by lookup-with-fallback
below.
"""

import logging

import bpy

LOGGER = logging.getLogger("nshade.bridge")

MIN_BLENDER = (4, 3, 0)
# Feature probes still let the addon run on 3.6; it is simply not the target.
LEGACY_MIN_BLENDER = (3, 6, 0)

# Colour-space names, most specific first. Blender renamed the OCIO config in
# 4.0 and a custom config can rename it again, so every lookup tries a list.
NON_COLOR_NAMES = ("Non-Color", "Non Colour", "Non-Color Data", "Raw")
SRGB_NAMES = ("sRGB", "sRGB IEC61966-2.1", "Linear sRGB", "Filmic sRGB")


def blender_version():
    return tuple(bpy.app.version)


def version_string():
    return ".".join(str(part) for part in bpy.app.version)


def supported():
    return blender_version() >= LEGACY_MIN_BLENDER


def recommended():
    return blender_version() >= MIN_BLENDER


def numpy():
    """Blender ships numpy in every supported build.

    Returns None instead of raising so the panel can explain the problem
    rather than the addon failing to import.
    """
    try:
        import numpy  # noqa: WPS433 - deliberately late and optional
    except ImportError:
        return None
    return numpy


def job_running(kind):
    """True while a render or bake owns the depsgraph.

    Wrapped because `bpy.app.is_job_running` has grown its accepted strings
    over time; an unknown name raises instead of returning False.
    """
    probe = getattr(bpy.app, "is_job_running", None)
    if probe is None:
        return False
    try:
        return bool(probe(kind))
    except (TypeError, ValueError):
        return False


def fresh_image(name, width, height, bits=8):
    """Create the image a streamed map belongs in.

    8-bit data goes into a byte buffer: a 2048x2048 image costs 16 MiB of
    Blender-side storage instead of 64 MiB, which matches the frame the host
    sends.  16-bit data goes into a float buffer with half precision turned
    off, because half floats (10-bit mantissa) would throw away most of the
    height precision the transfer exists for.
    """
    sixteen = int(bits) == 16
    kwargs = {"alpha": False, "float_buffer": sixteen}
    try:
        image = bpy.data.images.new(name, width, height, is_data=True, **kwargs)
    except TypeError:
        image = bpy.data.images.new(name, width, height, **kwargs)
    if sixteen and hasattr(image, "use_half_precision"):
        image.use_half_precision = False
    mark_space(image, "linear")
    image.use_fake_user = False
    return image


def mark_space(image, space="linear"):
    """Tag a map as data (Non-Color) or colour (sRGB).

    Reading a normal map as sRGB bends every vector, and shading a base colour
    as linear washes it out; this is the single most important setting the
    addon applies to a streamed image.
    """
    settings = getattr(image, "colorspace_settings", None)
    candidates = SRGB_NAMES if str(space).lower() == "srgb" else NON_COLOR_NAMES
    if settings is not None:
        for candidate in candidates:
            try:
                settings.name = candidate
                break
            except TypeError:
                continue
    try:
        image.alpha_mode = "STRAIGHT" if str(space).lower() == "srgb" else "CHANNEL_PACKED"
    except (AttributeError, TypeError):
        pass


def image_size(image):
    """(width, height) as plain ints, or (0, 0) for a dead datablock."""
    try:
        size = image.size
    except ReferenceError:
        return (0, 0)
    if len(size) < 2:
        return (0, 0)
    return int(size[0]), int(size[1])


def principled_node(tree):
    """Find the Principled BSDF by type, never by name.

    Socket and node names are translated in the UI; the `type` is stable.
    """
    if tree is None:
        return None
    for node in tree.nodes:
        if node.type == "BSDF_PRINCIPLED":
            return node
    return None


def output_node(tree):
    if tree is None:
        return None
    for node in tree.nodes:
        if node.type == "OUTPUT_MATERIAL":
            return node
    return None


def socket(node, name, direction="input"):
    """Look up a socket by name without assuming it exists.

    `node.inputs[name]` raises KeyError when a socket was renamed between
    versions; `.get()` keeps the caller honest about the miss.
    """
    collection = node.inputs if direction == "input" else node.outputs
    getter = getattr(collection, "get", None)
    if getter is None:
        return None
    return getter(name)


def socket_by_identifier(node, identifier, direction="input"):
    """Look up a socket by its stable identifier ("A_Color", "Result_Color")."""
    collection = node.inputs if direction == "input" else node.outputs
    for item in collection:
        if getattr(item, "identifier", None) == identifier:
            return item
    return None


def node(tree, name, node_type, fallback_label="", location=None):
    """Find or create a node, addressing it by a stable internal name.

    An existing node is returned untouched, including its position: the user
    may have arranged the tree, and a material update must not shuffle it.
    New nodes are placed at `location` so a generated graph is readable.

    A same-named node of a different type is a real conflict: renaming or
    reusing it would corrupt the user's tree, so it is reported instead of
    being silently overwritten.
    """
    existing = tree.nodes.get(name)
    if existing is not None:
        current = getattr(existing, "bl_idname", None)
        if current is not None and current != node_type:
            raise ValueError(
                "node %s is a %s, not a %s; rename it or pick another material"
                % (name, current, node_type)
            )
        return existing
    created = tree.nodes.new(node_type)
    created.name = name
    if fallback_label:
        created.label = fallback_label
    if location is not None:
        created.location = location
    return created


def log(level, message, *args):
    LOGGER.log(level, message, *args)
