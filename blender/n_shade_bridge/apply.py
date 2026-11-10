"""The only module that touches bpy image data.

Called exclusively from the timer callback on Blender's main thread.  The
worker thread hands over finished byte buffers and never sees a datablock.

Memory and speed rules encoded here:

* one reusable Image datablock per map, resized in place instead of recreated,
  so Blender never accumulates orphaned buffers and node links never break;
* 8-bit storage (`float_buffer=False`) for colour and mask data, which is a
  quarter of the float cost and matches the 8-bit frame the host sends;
* 16-bit frames land in a full-precision float buffer, because half floats
  would throw away the low bits the height field is transferred for;
* one temporary float copy per frame, deleted before returning;
* the uint8 -> float32 conversion is a single numpy ufunc pass
  (`np.multiply(..., dtype=np.float32)`), not `astype` followed by a second
  multiply: half the memory traffic of the obvious loop;
* rows arrive top-down (the host's native order, the same one a PNG uses) and
  are written bottom-up, which is what Blender's pixel buffer expects.  The
  reversal happens inside that same ufunc pass, so it costs nothing extra;
* no pixel reads, ever.  `image.pixels[:]` would materialise four Python floats
  per texel and dwarf everything else in this addon.

Disk safety: generated images with users are serialised into the `.blend`, so
a handful of 4K maps would add a gigabyte to every save (and again to the
`.blend1` backup).  `shrink_for_save()` drops every streamed buffer to 1x1
before a save and `restore_after_save()` asks for them again afterwards, so the
file stays small and the images come back a fraction of a second later.
"""

import time

import bpy

from . import catalog, compat

PREFIX = "NS_"
NORMAL_IMAGE = PREFIX + "normal"
HEIGHT_IMAGE = PREFIX + "height"

# map id -> datablock we created (or adopted) in this session.
_BOUND = {}
# map id -> sample width the bound datablock was built for.
_BITS = {}
# map id -> (w, h) of images shrunk for a save, restored by the next pull.
_SHRUNK = {}


def image_name(map_id):
    return PREFIX + str(map_id)


def _alive(image):
    """A datablock from a previously loaded file raises on any access."""
    try:
        image.name
    except ReferenceError:
        return False
    return True


def image_for(map_id):
    """The live image for a map, or None if it has not been received yet."""
    name = image_name(map_id)
    image = _BOUND.get(map_id)
    if image is not None and _alive(image):
        return image
    _BOUND.pop(map_id, None)
    _BITS.pop(map_id, None)
    image = bpy.data.images.get(name)
    # A user's file-backed image with the same name is not ours to hand out.
    if image is None or image.source != "GENERATED":
        return None
    _BOUND[map_id] = image
    return image


def ensure_image(map_id, width, height, bits=8, space="linear"):
    """Return our image datablock, sized and tagged correctly, ready to write."""
    name = image_name(map_id)
    image = _BOUND.get(map_id)
    if image is not None and not _alive(image):
        image = None
        _BOUND.pop(map_id, None)
        _BITS.pop(map_id, None)

    # A change between 8- and 16-bit needs a different storage type, and
    # `float_buffer` cannot be flipped on an existing datablock.
    if image is not None and _BITS.get(map_id, 8) != int(bits):
        _remove(image)
        _BOUND.pop(map_id, None)
        _BITS.pop(map_id, None)
        image = None

    if image is None:
        candidate = bpy.data.images.get(name)
        # Never hijack a user's file-backed image that happens to share a name.
        if candidate is not None and candidate.source != "GENERATED":
            candidate = None
        image = candidate or compat.fresh_image(name, width, height, bits)
        _BOUND[map_id] = image
        _BITS[map_id] = int(bits)

    if compat.image_size(image) != (width, height):
        # In-place resize: node links and material references stay valid, and
        # there is only ever one buffer per map.
        image.scale(width, height)
    compat.mark_space(image, space)
    return image


def apply_map(map_id, width, height, bits, payload, space="linear"):
    """Upload one frame. Returns the time spent, in milliseconds."""
    np = compat.numpy()
    if np is None:
        raise RuntimeError("this Blender build has no numpy; cannot stream images")
    started = time.perf_counter()

    image = ensure_image(map_id, width, height, bits, space)

    # `flat` is a zero-copy view on the received bytearray; `pixels` is the one
    # unavoidable float copy that Image.pixels.foreach_set() needs. Doing the
    # integer-to-float conversion, the 1/255 scale and the vertical flip in a
    # single ufunc keeps it to one read and one write pass over the buffer.
    texels = width * height * 4
    if int(bits) == 16:
        flat = np.frombuffer(payload, dtype="<u2")
        if flat.size != texels:
            raise ValueError(
                "frame carries %d values, %dx%d RGBA16 needs %d"
                % (flat.size, width, height, texels)
            )
        scale = np.float32(1.0 / 65535.0)
    else:
        flat = np.frombuffer(payload, dtype=np.uint8)
        if flat.size != texels:
            raise ValueError(
                "frame carries %d values, %dx%d RGBA8 needs %d"
                % (flat.size, width, height, texels)
            )
        scale = np.float32(1.0 / 255.0)

    # Blender stores pixels bottom-left first; the host sends rows top-down.
    # The reversal is a strided view, and reshape(-1) keeps the ufunc output a
    # flat C-contiguous buffer, which is exactly what foreach_set wants.
    rows = flat.reshape(height, width * 4)[::-1]
    pixels = np.multiply(rows, scale, dtype=np.float32).reshape(-1)

    image.pixels.foreach_set(pixels)
    image.update()
    image.update_tag()

    # Drop both references before returning so the peak is released as soon as
    # the frame is on the GPU-side buffer, not at the next garbage collection.
    del pixels
    del flat
    return (time.perf_counter() - started) * 1000.0


def has_any():
    return bool(_BOUND)


def memory_bytes():
    """Approximate RAM held by the streamed images.

    8-bit images are stored as RGBA32 in Blender, 16-bit ones as four floats.
    This is what the panel reports instead of the cumulative transfer size,
    which says nothing about what is actually in memory.
    """
    total = 0
    for map_id in list(_BOUND):
        image = image_for(map_id)
        if image is None:
            continue
        width, height = compat.image_size(image)
        if width <= 1 and height <= 1:
            continue
        per_pixel = 16 if getattr(image, "is_float", False) else 4
        total += width * height * per_pixel
    return total


def shrink_for_save():
    """Drop every streamed buffer to 1x1 so a save stays small.

    The datablock and every node link stay intact - only the pixels go - so
    nothing reconnects on the other side.  Returns the map ids to refill.
    """
    _SHRUNK.clear()
    for map_id in list(_BOUND):
        image = image_for(map_id)
        if image is None:
            continue
        width, height = compat.image_size(image)
        if width <= 1 and height <= 1:
            continue
        try:
            image.scale(1, 1)
        except (RuntimeError, ReferenceError):
            continue
        _SHRUNK[map_id] = (width, height)
    return dict(_SHRUNK)


def shrunk():
    """Map ids waiting to be refilled after a save, with their old size."""
    return dict(_SHRUNK)


def clear_shrunk():
    _SHRUNK.clear()


def bound_maps():
    """Map ids that currently have a live image, newest state included."""
    live = []
    for map_id in list(_BOUND):
        if image_for(map_id) is not None:
            live.append(map_id)
    return live


def forget():
    """Drop cached references without touching the datablocks.

    Used after a file load: the old datablocks are gone, and holding Python
    references to freed IDs is the classic way an addon crashes Blender.
    """
    _BOUND.clear()
    _BITS.clear()
    _SHRUNK.clear()


def purge():
    """Delete every live image to hand its memory back."""
    for image in list(_BOUND.values()):
        _remove(image)
    _BOUND.clear()
    _BITS.clear()
    _SHRUNK.clear()
    for image in list(bpy.data.images):
        if image.name.startswith(PREFIX) and image.source == "GENERATED":
            _remove(image)


def _remove(image):
    try:
        bpy.data.images.remove(image, do_unlink=True)
    except (ReferenceError, RuntimeError):
        pass


def describe(map_id):
    """Short "1024x1024 · 16-bit" line for the panel."""
    image = image_for(map_id)
    if image is None:
        return ""
    width, height = compat.image_size(image)
    bits = _BITS.get(map_id, 8)
    return "%dx%d · %d-bit" % (width, height, bits)


def map_label(map_id):
    return catalog.label_for(map_id)
