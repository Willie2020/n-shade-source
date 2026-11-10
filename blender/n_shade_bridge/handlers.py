"""The one timer that drives the bridge.

Blender calls this callback on the main thread, which is the only place where
touching `bpy.data` is legal.  Everything the worker thread produced is drained
here, exactly one image per tick, and every network write is queued rather than
performed, so the callback stays short even when the host is slow.

Pacing: at most one request is in flight, and every request may carry a
`wait_ms` the host honours.  A live request therefore returns the moment the
document changes (or after its wait expires with "unchanged"), which turns the
old 10 Hz poll into an event-driven stream with the same idle cost: a handful of
100-byte frames per second and no evaluation at all while nobody is painting.

Failure modes this file is built to survive:

* an exception inside a timer callback.  Blender silently unregisters the
  timer when the callback raises, which would look like "the addon stopped
  working" with no message.  Every tick is wrapped and returns an interval.
* a file load.  `load_post` invalidates datablocks from the previous file;
  holding those Python references is the classic way an addon crashes Blender,
  so the cached images are dropped before the next frame is applied.
* a host that goes away.  The in-flight request times out, the socket thread
  reports the closure, and an idle tick reconnects with backoff.
"""

import time
import traceback

import bpy

from . import apply, catalog, compat, material, prefs, state
from .client import CLIENT

TICK = 0.05
# Longer than a live wait plus the host's evaluation, short enough that a
# dropped request is retried instead of stalling the stream.
REPLY_TIMEOUT = 2.5
# A mesh import answers after the host's importer has actually read the file.
MESH_REPLY_TIMEOUT = 4.0
RECONNECT_DELAY = 3.0
# How long the host may hold a live request open.
LIVE_WAIT = 0.35
# Idle notification request when streaming is off. The host answers as soon as
# the app pushes maps, so "Send to Blender" still lands immediately.
WATCH_WAIT = 0.35

# Maps whose precision the user asked to keep at 16 bit.
SIXTEEN_BIT_MAPS = ("height", "pbr_height", "pbr_displacement")

# False after an explicit Disconnect so the addon does not immediately undo it.
_auto = True
_next_attempt = 0.0

# Manual pulls waiting for their turn: (map_id, size, bits, force).
MAX_QUEUE = 32
_queue = []
# map_id -> size the user explicitly pulled at. The live stream never
# downsamples an image the user asked for at full resolution.
_pinned = {}
# Earliest time the next live request may be sent (frame-rate cap).
_next_live_at = 0.0
# Signature of the last material build. Pixel updates keep it stable, so the
# node tree is rebuilt only when a map, option or target material changes.
_last_wire_signature = None


def start():
    if not bpy.app.timers.is_registered(_pump):
        bpy.app.timers.register(_pump, first_interval=TICK, persistent=True)
    if _on_load_post not in bpy.app.handlers.load_post:
        bpy.app.handlers.load_post.append(_on_load_post)
    if _on_save_pre not in bpy.app.handlers.save_pre:
        bpy.app.handlers.save_pre.append(_on_save_pre)
    if _on_save_post not in bpy.app.handlers.save_post:
        bpy.app.handlers.save_post.append(_on_save_post)


def shutdown():
    global _auto
    _auto = False
    _queue.clear()
    _pinned.clear()
    reset_wire_cache()
    CLIENT.stop()
    for handlers_list, callback in (
        (bpy.app.handlers.load_post, _on_load_post),
        (bpy.app.handlers.save_pre, _on_save_pre),
        (bpy.app.handlers.save_post, _on_save_post),
    ):
        if callback in handlers_list:
            handlers_list.remove(callback)
    if bpy.app.timers.is_registered(_pump):
        bpy.app.timers.unregister(_pump)


def resume_auto():
    global _auto
    _auto = True


def suspend_auto():
    global _auto
    _auto = False


# ---------------------------------------------------------------------------
# Public request API (operators)
# ---------------------------------------------------------------------------


def request_now(map_id=None, force=True):
    """Queue one map at the pull size. Returns the map id, or None."""
    map_id = map_id or active_map()
    if not map_id:
        return None
    if len(_queue) < MAX_QUEUE:
        _queue.append((map_id, pull_size(), bits_for(map_id), force))
    return map_id


def request_many(map_ids):
    """Queue several maps for a full-resolution pull."""
    queued = 0
    # A bounded queue: a stuck session must not grow without limit while the
    # user keeps pressing Get all.
    for map_id in map_ids:
        if len(_queue) >= MAX_QUEUE:
            break
        if request_now(map_id):
            queued += 1
    return queued


def clear_queue():
    del _queue[:]


def pinned():
    return dict(_pinned)


def unpin():
    _pinned.clear()


# ---------------------------------------------------------------------------
# Settings helpers
# ---------------------------------------------------------------------------


def settings():
    scene = getattr(bpy.context, "scene", None)
    return getattr(scene, "n_shade_bridge", None) if scene is not None else None


def _value(name, fallback=None):
    data = settings()
    if data is None:
        return fallback
    try:
        return getattr(data, name)
    except AttributeError:
        return fallback


def active_map():
    """The map the live stream follows."""
    current = str(_value("live_map", "normal") or "normal")
    known = {info.id for info in state.STATE.catalog}
    if known and current not in known:
        # Fall back to the document normal, which every host serves.
        if "normal" in known:
            return "normal"
        return next(iter(sorted(known)))
    return current


def pull_size():
    try:
        return max(0, int(_value("pull_size", "2048")))
    except (TypeError, ValueError):
        return 2048


def live_size():
    try:
        return max(0, int(_value("live_size", "1024")))
    except (TypeError, ValueError):
        return 1024


def bits_for(map_id):
    if map_id not in SIXTEEN_BIT_MAPS:
        return 8
    return 16 if bool(_value("bits16", True)) else 8


def wanted():
    return catalog.split_selection(_value("wanted", ""))


def set_wanted(values):
    data = settings()
    if data is None:
        return
    data.wanted = catalog.join_selection(values, state.STATE.catalog)


def toggle_wanted(map_id):
    values = wanted()
    if map_id in values:
        values.remove(map_id)
    else:
        values.append(map_id)
    set_wanted(values)
    return wanted()


def _space_for(map_id):
    for info in state.STATE.catalog:
        if info.id == map_id:
            return info.space
    return "linear"


def _max_size(first, second):
    """Largest of two size requests, where 0 means native."""
    if first == 0 or second == 0:
        return 0
    return max(first, second)


# ---------------------------------------------------------------------------
# The timer
# ---------------------------------------------------------------------------


def _pump():
    global _next_attempt
    try:
        if not compat.supported():
            return TICK * 5

        if prefs.value("pause_during_render", True) and (
            compat.job_running("RENDER") or compat.job_running("BAKE")
        ):
            # Keep the timer alive and the link warm, but do not touch the
            # depsgraph while a job owns it.  Control messages are drained so
            # the queues cannot grow; a waiting image frame stays queued (at
            # most one) and is applied on the first tick after the job ends.
            _handle_control(CLIENT.drain_control())
            CLIENT.ping()
            return TICK * 2

        _consume()

        if CLIENT.connected:
            if state.STATE.pending_seq and _expired():
                if state.STATE.pending_kind == "mesh":
                    # Only the 0.2.0 host knows OP_PUSH_MESH; an older one drops
                    # the frame silently, so say what to do instead of nothing.
                    state.STATE.note_notice(
                        "No answer to the mesh request - N-shade 0.2.0 or newer "
                        "must be running"
                    )
                # The host dropped or is still holding the request; ask again.
                state.STATE.reset_request()
            if not state.STATE.pending_seq:
                _pump_requests()
        elif _auto and not CLIENT.running:
            now = time.monotonic()
            if now >= _next_attempt:
                _next_attempt = now + RECONNECT_DELAY
                _try_connect()
    except Exception:
        compat.log(40, "bridge: pump failed\n%s", traceback.format_exc())
        return 0.5
    return TICK


def _expired():
    started = state.STATE.pending_since
    if not started:
        return False
    limit = MESH_REPLY_TIMEOUT if state.STATE.pending_kind == "mesh" else REPLY_TIMEOUT
    return (time.monotonic() - started) > limit


def _pump_requests():
    global _next_live_at
    # 1. Manual pulls win over the live stream.
    if _queue:
        if state.STATE.pending_kind == "watch":
            # Supersede a notification request; the host keeps only the newest
            # pending request, so this cannot strand the watch.
            state.STATE.reset_request()
        map_id, size, bits, force = _queue.pop(0)
        if CLIENT.request_map(
            map_id, size, bits=bits, wait=0.0, force=force
        ) is None:
            _queue.insert(0, (map_id, size, bits, force))
        else:
            _pinned[map_id] = size
        return

    # 2. The live stream, if the user asked for it.
    streaming = bool(_value("live", True))
    map_id = active_map()
    if streaming and map_id:
        # A frame-rate cap keeps a fast host from uploading textures faster
        # than anyone can see them.
        now = time.monotonic()
        if now < _next_live_at:
            return
        size = live_size()
        pinned = _pinned.get(map_id)
        if pinned is not None:
            # Never downsample what the user pulled at full resolution.
            size = _max_size(size, pinned)
        if CLIENT.request_map(
            map_id,
            size,
            bits=bits_for(map_id),
            wait=LIVE_WAIT,
            force=False,
        ) is None:
            return
        _next_live_at = now + 1.0 / max(1.0, float(prefs.value("max_fps", 8) or 8))
        return

    # 3. Otherwise keep a notification request open so a push from the app
    #    reaches this session even with streaming switched off.
    CLIENT.watch(state.STATE.epoch, wait=WATCH_WAIT)


def _try_connect():
    """Connect, re-reading the pairing file every attempt.

    The host rewrites bridge.json on every start (new token, possibly a new
    port) and deletes it when the bridge is switched off, so the file - not a
    remembered token - is the source of truth for a local session.  That makes
    "start Blender first, then N-shade" work without the user re-pairing.
    """
    if prefs.value("auto_connect", True):
        from . import pairing, protocol

        pairing.apply_to(prefs.get_prefs(), protocol.PROTOCOL_VERSION)
    token = prefs.value("token", "")
    if not token:
        return False
    port = int(prefs.value("port", 47100))
    return CLIENT.start("127.0.0.1", port, token)


# ---------------------------------------------------------------------------
# Events
# ---------------------------------------------------------------------------


def _handle_control(events):
    for event in events:
        kind = event.get("kind")
        if kind == "error":
            state.STATE.note_error(event.get("message", "host error"))
        elif kind == "notice":
            state.STATE.note_notice(event.get("message", ""))
        elif kind == "hello":
            # A fresh session invalidates any request from the previous one.
            state.STATE.reset_request()
        elif kind == "published":
            _on_published(event.get("maps") or ())


def _consume():
    for event in CLIENT.drain():
        kind = event.get("kind")
        if kind == "map":
            _apply(event)
        elif kind == "published":
            _on_published(event.get("maps") or ())
        else:
            _handle_control([event])


def _on_published(map_ids):
    """The app pushed maps: fetch them at full resolution, whatever the mode."""
    size = pull_size()
    for map_id in map_ids:
        if map_id:
            _queue.append((map_id, size, bits_for(map_id), True))
    if map_ids:
        state.STATE.note_notice(
            "N-shade pushed %d map(s)" % len(map_ids)
        )


def _apply(event):
    map_id = event["map"]
    header = event.get("header") or {}
    try:
        elapsed = apply.apply_map(
            map_id,
            event["w"],
            event["h"],
            event["bits"],
            event["payload"],
            _space_for(map_id),
        )
    except (RuntimeError, ValueError) as exc:
        state.STATE.note_error(str(exc))
        compat.log(40, "bridge: could not apply a frame: %s", exc)
        return

    state.STATE.note_map(map_id, event["w"], event["h"], event["bits"], header, elapsed)
    if prefs.value("log_frames", False):
        info = state.STATE.info(map_id)
        compat.log(
            20,
            "bridge: %s %dx%d %d-bit in %.1f ms (host eval %.1f ms)",
            map_id,
            info["w"],
            info["h"],
            info["bits"],
            elapsed,
            info["eval_ms"],
        )
    if bool(_value("auto_wire", True)):
        _auto_wire()


def _live_images():
    """The map datablocks that are ready to wire, keyed by map id."""
    images = {}
    for map_id in apply.bound_maps():
        image = apply.image_for(map_id)
        if image is not None:
            images[map_id] = image
    return images


def _wire_key(images):
    """What a graph build depends on: map set, image datablocks and options."""
    name = str(_value("material", material.DEFAULT_MATERIAL) or material.DEFAULT_MATERIAL)
    return (
        name,
        bool(_value("auto_assign", True)),
        material.signature(
            images,
            strength=float(_value("strength", 1.0) or 1.0),
            use_orm=bool(_value("use_orm", True)),
            use_ao=bool(_value("use_ao", True)),
            bump_from_height=bool(_value("bump_from_height", True)),
        ),
    )


def _auto_wire():
    """Build the graph once per map/option change, not once per frame."""
    images = _live_images()
    if not images:
        return
    if _wire_key(images) == _last_wire_signature:
        return
    refresh_material(quiet=True)


def reset_wire_cache():
    """Forget what was wired; the next map arrival rebuilds the tree."""
    global _last_wire_signature
    _last_wire_signature = None


def refresh_material(quiet=False):
    """Rebuild the live material from whatever images exist."""
    global _last_wire_signature
    images = _live_images()
    if not images:
        if not quiet:
            state.STATE.note_error("No streamed maps yet")
        return None
    name = str(_value("material", material.DEFAULT_MATERIAL) or material.DEFAULT_MATERIAL)
    try:
        target = material.ensure_material(name)
        material.sync(
            target,
            images,
            strength=float(_value("strength", 1.0) or 1.0),
            use_orm=bool(_value("use_orm", True)),
            use_ao=bool(_value("use_ao", True)),
            bump_from_height=bool(_value("bump_from_height", True)),
        )
    except (RuntimeError, ValueError, KeyError) as exc:
        # Remember the failed key too: a name/type conflict is persistent, and
        # retrying it on every streamed frame would just repeat the same error.
        _last_wire_signature = _wire_key(images)
        if quiet:
            compat.log(30, "bridge: material update failed: %s", exc)
        else:
            state.STATE.note_error("Material update failed: %s" % exc)
        return None
    _last_wire_signature = _wire_key(images)
    if bool(_value("auto_assign", True)):
        obj = material.active_mesh(bpy.context)
        if obj is not None:
            material.apply_to_object(obj, target)
    return target


def _on_load_post(_arg):
    """Datablocks from the previous file are gone; drop every cached handle."""
    apply.forget()
    state.STATE.maps = {}
    state.STATE.reset_request()
    _pinned.clear()
    reset_wire_cache()


def _on_save_pre(*_args):
    """Drop the streamed pixels before the .blend is written.

    A generated image that a material references is serialised into the file,
    so a handful of 4K maps would add a gigabyte to every save.  Scaling to a
    single texel keeps the datablock and every node link while removing the
    payload; the pixels come back from the host right after the write.
    """
    if not prefs.value("strip_on_save", True):
        return
    try:
        shrunk = apply.shrink_for_save()
    except Exception as error:  # never block a save because of the addon
        compat.log(40, "bridge: could not shrink images for save: %s", error)
        return
    if shrunk:
        compat.log(20, "bridge: cleared %d streamed image(s) for the save", len(shrunk))


def _on_save_post(*_args):
    """Ask for the pixels that the save dropped."""
    shrunk = apply.shrunk()
    apply.clear_shrunk()
    for map_id, (width, height) in shrunk.items():
        size = max(width, height)
        if len(_queue) >= MAX_QUEUE:
            break
        _queue.append((map_id, size, bits_for(map_id), True))
