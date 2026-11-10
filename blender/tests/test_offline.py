"""Offline checks for the Blender add-on.

Blender is not available in CI, so this script stubs the parts of `bpy` the
add-on touches at import time and then exercises everything that is pure
Python: the wire codec, the catalogue, the state machine and the request
planner.  Anything that needs a real node tree or image buffer is out of scope
here and is covered by the manual checklist in `blender/README.md`.

Run it with the interpreter of your choice:

    python blender/tests/test_offline.py
"""

import os
import socket
import struct
import sys
import threading
import time
import types

# Importing the add-on must not leave __pycache__ in the shipping folder; that
# is exactly the duplicate-file clutter this suite exists to prevent.
sys.dont_write_bytecode = True

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)


# ---------------------------------------------------------------------------
# Minimal bpy stub
# ---------------------------------------------------------------------------


class _Props(object):
    def __getattr__(self, _name):
        def factory(*_args, **kwargs):
            return kwargs.get("default")

        return factory


class _Collection(list):
    def get(self, name, default=None):
        for item in self:
            if getattr(item, "name", None) == name:
                return item
        return default

    def new(self, *_args, **_kwargs):
        return types.SimpleNamespace(name="stub")

    def remove(self, *_args, **_kwargs):
        return None


class _Types(object):
    def __getattr__(self, name):
        return type(name, (object,), {"__annotations__": {}})


class _Timers(object):
    def __init__(self):
        self.registered = []

    def is_registered(self, fn):
        return fn in self.registered

    def register(self, fn, **_kwargs):
        self.registered.append(fn)

    def unregister(self, fn):
        if fn in self.registered:
            self.registered.remove(fn)


def _install_bpy():
    bpy = types.ModuleType("bpy")
    bpy.props = _Props()
    bpy.types = _Types()
    bpy.data = types.SimpleNamespace(
        images=_Collection(),
        materials=_Collection(),
    )
    bpy.app = types.SimpleNamespace(
        version=(4, 3, 0),
        timers=_Timers(),
        handlers=types.SimpleNamespace(
            load_post=[], save_pre=[], save_post=[]
        ),
        is_job_running=lambda _kind: False,
    )
    bpy.context = types.SimpleNamespace(
        scene=types.SimpleNamespace(
            n_shade_bridge=types.SimpleNamespace(
                live=True,
                live_map="normal",
                live_size="1024",
                pull_size="2048",
                bits16=True,
                wanted="normal,height",
                material="N-Shade Live",
                strength=1.0,
                auto_wire=False,
                auto_assign=True,
                use_orm=True,
                use_ao=True,
                bump_from_height=True,
            )
        ),
        preferences=types.SimpleNamespace(addons={}),
    )
    bpy.utils = types.SimpleNamespace(
        register_class=lambda cls: None,
        unregister_class=lambda cls: None,
    )
    sys.modules["bpy"] = bpy
    return bpy


_install_bpy()

from n_shade_bridge import (  # noqa: E402
    apply,
    catalog,
    compat,
    handlers,
    material,
    pairing,
    protocol,
    props,
    state,
)
from n_shade_bridge import material as material_mod  # noqa: E402
from n_shade_bridge.client import CLIENT  # noqa: E402

FAILURES = []


def check(name, condition, detail=""):
    if condition:
        print("  ok   %s" % name)
    else:
        print("  FAIL %s %s" % (name, detail))
        FAILURES.append(name)


# ---------------------------------------------------------------------------
# Protocol
# ---------------------------------------------------------------------------


def test_protocol():
    print("protocol")
    frame = protocol.encode(protocol.OP_GET_MAP, 7, {"map": "normal", "max_size": 1024})
    magic, op, _flags, hlen, plen, seq = protocol.HEADER.unpack(frame[: protocol.HEADER_SIZE])
    check("magic", magic == protocol.MAGIC)
    check("op", op == protocol.OP_GET_MAP)
    check("seq", seq == 7)
    check("frame length", len(frame) == protocol.HEADER_SIZE + hlen + plen)

    eight = protocol.map_header("normal", 4, 4)
    check("8-bit validate", protocol.validate_map(eight, b"\0" * (4 * 4 * 4)) == (4, 4, 8))
    sixteen = protocol.map_header("height", 4, 4, bits=16)
    check(
        "16-bit validate",
        protocol.validate_map(sixteen, b"\0" * (4 * 4 * 8)) == (4, 4, 16),
    )
    try:
        protocol.validate_map(sixteen, b"\0" * (4 * 4 * 4))
    except protocol.ProtocolError:
        check("short payload rejected", True)
    else:
        check("short payload rejected", False)

    try:
        protocol.validate_map(protocol.map_header("n", 4096, 4096), b"\0")
    except protocol.ProtocolError:
        check("oversized payload rejected", True)
    else:
        check("oversized payload rejected", False)

    # Pinned byte-for-byte: bridge.rs builds the same header by hand.
    exact = protocol.encode(protocol.OP_GET_MAP, 7, None, b"abc")
    check(
        "framing bytes match the host",
        exact
        == bytes(
            [
                0x51, 0x58, 0x4C, 0x31,  # "QXL1"
                0x10,                    # GET_MAP
                0x00,                    # flags
                0x00, 0x00,              # header length
                0x03, 0x00, 0x00, 0x00,  # payload length
                0x07, 0x00, 0x00, 0x00,  # seq
            ]
        )
        + b"abc",
        exact[:16],
    )


# ---------------------------------------------------------------------------
# Catalogue
# ---------------------------------------------------------------------------


def test_catalog():
    print("catalog")
    defaults = catalog.defaults()
    check("defaults cover every id", len(defaults) == len(catalog.ids()))
    check("ids unique", len(set(catalog.ids())) == len(catalog.ids()))

    parsed = catalog.parse(
        [
            {"id": "normal", "label": "Normal", "group": "document", "bits": 8, "ready": True},
            {"id": "normal", "label": "Duplicate"},
            {"id": "pbr_orm", "label": "ORM", "group": "pbr", "bits": 8, "ready": False},
            {"nope": 1},
            "junk",
        ]
    )
    check("parse drops duplicates", [entry.id for entry in parsed] == ["normal", "pbr_orm"])
    check("parse keeps readiness", [entry.ready for entry in parsed] == [True, False])

    ready = catalog.apply_readiness(parsed, ["pbr_orm"])
    check("readiness refresh", [entry.ready for entry in ready] == [False, True])

    values = catalog.split_selection("normal, pbr_orm ,,height")
    check("selection split", values == ["normal", "pbr_orm", "height"])
    check(
        "selection join keeps catalogue order",
        catalog.join_selection(["height", "normal"], parsed) == "normal,height",
    )
    check("empty selection", catalog.join_selection([]) == "")


# ---------------------------------------------------------------------------
# State
# ---------------------------------------------------------------------------


def test_state():
    print("state")
    snapshot = state.STATE
    snapshot.mark_connecting(47100)
    check("connecting status", "47100" in snapshot.status)
    snapshot.mark_connected("abc", "0.1.17", epoch=3, max_size=4096)
    check("connected", snapshot.connected and snapshot.phase == "connected")
    check("epoch stored", snapshot.epoch == 3)

    snapshot.begin_request(11, "map")
    check("request recorded", snapshot.pending_seq == 11 and snapshot.requests == 1)
    check("stale reply ignored", snapshot.finish_request(10) is False)
    check("matching reply accepted", snapshot.finish_request(11) is True)
    check("request cleared", snapshot.pending_seq == 0)

    snapshot.note_map("normal", 8, 8, 8, {"serial": 5}, 1.5)
    info = snapshot.info("normal")
    check("map recorded", info["w"] == 8 and info["serial"] == 5)
    check("map counted", snapshot.frames == 1 and snapshot.last_map == "normal")

    snapshot.mark_closed("test")
    check("closed", not snapshot.connected and snapshot.phase == "idle")


# ---------------------------------------------------------------------------
# Handlers / planning
# ---------------------------------------------------------------------------


def test_handlers():
    print("handlers")
    check("active map defaults to normal", handlers.active_map() == "normal")
    check("pull size reads the scene", handlers.pull_size() == 2048)
    check("live size reads the scene", handlers.live_size() == 1024)
    check("16-bit only for height", handlers.bits_for("height") == 16)
    check("8-bit for normals", handlers.bits_for("normal") == 8)

    handlers.set_wanted(["height", "normal"])
    check("wanted round trip", set(handlers.wanted()) == {"height", "normal"})
    handlers.toggle_wanted("curvature")
    check("toggle adds", "curvature" in handlers.wanted())
    handlers.toggle_wanted("curvature")
    check("toggle removes", "curvature" not in handlers.wanted())

    handlers.clear_queue()
    handlers.request_many(["normal", "height"])
    check("queue filled", len(handlers._queue) == 2)
    check(
        "pull uses 16-bit for height",
        handlers._queue[1][2] == 16,
    )
    handlers.clear_queue()
    check("queue cleared", not handlers._queue)

    check("max size with native", handlers._max_size(1024, 0) == 0)
    check("max size picks larger", handlers._max_size(1024, 2048) == 2048)


# ---------------------------------------------------------------------------
# Material roles
# ---------------------------------------------------------------------------


def test_material():
    print("material")
    marker = object()
    roles = material.pick({"normal": marker, "pbr_base_color": marker})
    check("normal chosen", roles["normal"][0] == "normal")
    check("base colour chosen", roles["base_color"][0] == "pbr_base_color")
    check("missing role absent", "roughness" not in roles)

    roles = material.pick({"pbr_normal": marker, "pbr_height": marker})
    check("pbr normal preferred over height", "normal" in roles)
    check("height available for bump", roles["height"][0] == "pbr_height")


class _LegacyMixNode(object):
    """Kept small: `test_material_graph` covers the real Mix wiring."""

    name = "mix"
    type = "MIX"

    def __init__(self):
        self.inputs = _FakeSockets(
            [
                _FakeSocket("Factor", "Factor_Float", "VALUE"),
                _FakeSocket("A", "A_Color", "RGBA"),
                _FakeSocket("B", "B_Color", "RGBA"),
            ]
        )
        self.outputs = _FakeSockets([_FakeSocket("Result", "Result_Color", "RGBA")])


def test_mix_sockets():
    print("mix sockets")
    mix = _LegacyMixNode()
    check(
        "factor by identifier",
        material._mix_socket(mix, "Factor", "Float").identifier == "Factor_Float",
    )
    check(
        "colour input by identifier",
        material._mix_socket(mix, "A", "Color").identifier == "A_Color",
    )
    check(
        "result by identifier",
        material._mix_socket(mix, "Result", "Color", direction="output").identifier
        == "Result_Color",
    )


# ---------------------------------------------------------------------------
# Pairing
# ---------------------------------------------------------------------------


def test_pairing():
    print("pairing")
    path = pairing.config_path()
    check("pairing path is absolute", os.path.isabs(path))
    check("pairing name", path.endswith("bridge.json") or path.endswith("n-shade-bridge.json"))


# ---------------------------------------------------------------------------
# Wire round trip against a fake host
# ---------------------------------------------------------------------------


def _read_frame_blocking(conn, timeout=3.0):
    """protocol.read_frame, retried across the client's short idle timeout."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        frame = protocol.read_frame(conn)
        if frame is not None:
            return frame
    raise RuntimeError("fake host timed out")


def test_wire_round_trip():
    print("wire round trip")
    server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    server.bind(("127.0.0.1", 0))
    server.listen(1)
    port = server.getsockname()[1]
    received = []
    errors = []
    ready = threading.Event()

    def host():
        try:
            server.settimeout(5.0)
            conn, _address = server.accept()
            conn.settimeout(0.05)
            op, _flags, seq, header, _payload = _read_frame_blocking(conn)
            if op != protocol.OP_HELLO or header.get("protocol") != protocol.PROTOCOL_VERSION:
                raise AssertionError("expected HELLO with the current protocol")
            if header.get("token") != "test-token":
                raise AssertionError("token missing from HELLO")
            conn.sendall(
                protocol.encode(
                    protocol.OP_HELLO_OK,
                    seq,
                    {
                        "protocol": protocol.PROTOCOL_VERSION,
                        "app": "n-shade",
                        "app_version": "test",
                        "session": "abcd1234",
                        "max_size": 4096,
                        "epoch": 0,
                    },
                )
            )
            catalogue = [
                {"id": "normal", "label": "Normal", "group": "document", "bits": 8, "ready": True},
                {"id": "pbr_roughness", "label": "Roughness", "group": "pbr", "bits": 8, "ready": True},
            ]
            conn.sendall(protocol.encode(protocol.OP_CATALOG, 0, {"maps": catalogue}))
            ready.set()
            deadline = time.monotonic() + 6.0
            pulls = [0]
            while time.monotonic() < deadline:
                frame = protocol.read_frame(conn)
                if frame is None:
                    continue
                op, _flags, got_seq, header, _payload = frame
                received.append((op, header))
                if op == protocol.OP_GET_MAP:
                    pulls[0] += 1
                    # The real host answers every request. The third one also
                    # reports a push that happened while it was in flight.
                    if pulls[0] == 3:
                        conn.sendall(
                            protocol.encode(
                                protocol.OP_UNCHANGED,
                                got_seq,
                                {
                                    "serial": 1,
                                    "epoch": 1,
                                    "published": ["pbr_roughness"],
                                },
                            )
                        )
                    else:
                        conn.sendall(
                            protocol.encode(
                                protocol.OP_UNCHANGED,
                                got_seq,
                                {"serial": 1, "epoch": 0},
                            )
                        )
                if op == protocol.OP_WATCH:
                    conn.sendall(
                        protocol.encode(
                            protocol.OP_PUBLISHED,
                            0,
                            {"maps": ["pbr_roughness"], "epoch": 1, "serial": 9},
                        )
                    )
            conn.close()
        except ConnectionError:
            pass
        except Exception as exc:  # surfaced in the main thread below
            errors.append(exc)
        finally:
            try:
                server.close()
            except OSError:
                pass

    thread = threading.Thread(target=host, daemon=True)
    thread.start()

    CLIENT.start("127.0.0.1", port, "test-token")
    deadline = time.monotonic() + 3.0
    while time.monotonic() < deadline and not CLIENT.connected:
        handlers._pump()
        time.sleep(0.02)
    check("client connected", CLIENT.connected)
    check("catalog reached the state", len(state.STATE.catalog) == 2)
    check(
        "catalog readiness",
        {entry.id: entry.ready for entry in state.STATE.catalog}.get("normal") is True,
    )

    # Let the pump ask for the live map, then answer a push.
    for _ in range(12):
        handlers._pump()
        time.sleep(0.02)
    maps = [header for op, header in list(received) if op == protocol.OP_GET_MAP]
    check("host received a GET_MAP", bool(maps))
    if maps:
        check("live request is the active map", maps[0].get("map") == "normal")
        check("live request uses the live size", maps[0].get("max_size") == 1024)
        check("live request is a long poll", maps[0].get("wait_ms") == 350)
        check("live request is not forced", maps[0].get("force") is False)

    deadline = time.monotonic() + 3.0
    while time.monotonic() < deadline:
        handlers._pump()
        if state.STATE.published:
            break
        time.sleep(0.02)
    check("publish notification arrived", state.STATE.published == ["pbr_roughness"])
    check("epoch adopted", state.STATE.epoch == 1)

    for _ in range(10):
        handlers._pump()
        time.sleep(0.02)
    pushed = [header for op, header in list(received) if op == protocol.OP_GET_MAP]
    check(
        "a push turns into a forced full-size pull",
        any(
            header.get("map") == "pbr_roughness"
            and header.get("force") is True
            and header.get("max_size") == 2048
            for header in pushed
        ),
        pushed,
    )

    CLIENT.stop()
    thread.join(timeout=1.0)
    check("no host-side error", not errors, errors)


# ---------------------------------------------------------------------------
# Pixel upload (needs numpy, which Blender ships)
# ---------------------------------------------------------------------------


class _FakePixels(object):
    def __init__(self, count):
        self.values = None
        self.count = count

    def foreach_set(self, values):
        self.values = list(values)
        assert len(values) == self.count, "pixel count mismatch"


class _FakeImage(object):
    def __init__(self, name, width, height, float_buffer=False):
        self.name = name
        self.size = (width, height)
        self.source = "GENERATED"
        self.float_buffer = float_buffer
        self.is_float = float_buffer
        self.use_half_precision = True
        self.use_fake_user = True
        self.alpha_mode = "STRAIGHT"
        self.colorspace_settings = types.SimpleNamespace(name="sRGB")
        self.pixels = _FakePixels(width * height * 4)
        self.updates = 0
        self.scales = 0

    def scale(self, width, height):
        self.size = (width, height)
        self.pixels = _FakePixels(width * height * 4)
        self.scales += 1

    def update(self):
        self.updates += 1

    def update_tag(self):
        return None


class _FakeImages(object):
    def __init__(self):
        self.items = {}

    def get(self, name, default=None):
        return self.items.get(name, default)

    def new(self, name, width, height, **kwargs):
        image = _FakeImage(name, width, height, kwargs.get("float_buffer", False))
        self.items[name] = image
        return image

    def remove(self, image, do_unlink=False):
        self.items.pop(getattr(image, "name", ""), None)

    def __iter__(self):
        return iter(list(self.items.values()))


def test_pixel_upload():
    print("pixel upload")
    import bpy

    try:
        import numpy  # noqa: F401
    except ImportError:
        print("  skip numpy is not installed")
        return

    apply.forget()
    bpy.data.images = _FakeImages()

    payload = bytes([255, 0, 128, 255, 0, 255, 64, 255, 10, 20, 30, 255, 1, 2, 3, 255])
    elapsed = apply.apply_map("normal", 2, 2, 8, payload, "linear")
    image = apply.image_for("normal")
    check("image created", image is not None and image.size == (2, 2))
    check("8-bit stays a byte buffer", image.float_buffer is False)
    check("non-color tag", image.colorspace_settings.name == "Non-Color")
    check("apply time reported", elapsed >= 0.0)
    pixels = image.pixels.values
    # Rows arrive top-down and must land bottom-up: the last input row is the
    # first output row.  This is the vertical flip Blender needs.
    check("bottom row written first", abs(pixels[0] - 10.0 / 255.0) < 1e-6)
    check("bottom row green", abs(pixels[1] - 20.0 / 255.0) < 1e-6)
    check("bottom row blue", abs(pixels[2] - 30.0 / 255.0) < 1e-6)
    check("alpha written", abs(pixels[3] - 1.0) < 1e-6)
    check("top row written last", abs(pixels[8] - 255.0 / 255.0) < 1e-6)
    check("top row green", abs(pixels[9]) < 1e-6)

    # Memory accounting and the save guard: shrinking keeps the datablock and
    # its links but hands the pixels back.
    check("memory matches the buffers", apply.memory_bytes() == 2 * 2 * 4)
    shrunk = apply.shrink_for_save()
    check("shrink reports the size", shrunk.get("normal") == (2, 2))
    check("shrink keeps the datablock", apply.image_for("normal") is image)
    check("shrink drops the pixels", apply.memory_bytes() == 0)
    check("shrunk map is queued for refill", "normal" in apply.shrunk())
    apply.clear_shrunk()
    check("clear_shrunk empties the queue", not apply.shrunk())

    # A resize reuses the same datablock: node links must survive.
    same = apply.image_for("normal")
    scales_before = same.scales
    apply.apply_map("normal", 4, 2, 8, bytes([7, 7, 7, 255] * 8), "linear")
    check("resize reuses the datablock", apply.image_for("normal") is same)
    check("resize happened in place", same.size == (4, 2) and same.scales == scales_before + 1)

    # 16-bit needs a float buffer, so the datablock is replaced.
    wide = b"".join(
        int(value).to_bytes(2, "little")
        for value in (65535, 0, 32768, 65535) * 8
    )
    apply.apply_map("height", 4, 2, 16, wide, "linear")
    height = apply.image_for("height")
    check("16-bit uses a float buffer", height.float_buffer is True)
    check("half precision disabled", height.use_half_precision is False)
    height_pixels = height.pixels.values
    check("16-bit full scale", abs(height_pixels[0] - 1.0) < 1e-6)
    check("16-bit midpoint", abs(height_pixels[2] - 0.5) < 1e-3)

    # Switching the same map back to 8 bit replaces the float buffer.
    apply.apply_map("height", 2, 2, 8, bytes([0, 0, 0, 255] * 4), "linear")
    check("bit depth change recreates", apply.image_for("height").float_buffer is False)

    apply.purge()
    check("purge removes every streamed image", not apply.has_any())


# ---------------------------------------------------------------------------
# Material graph (fake node tree)
# ---------------------------------------------------------------------------


class _FakeSocket(object):
    def __init__(self, name, identifier=None, socket_type="VALUE"):
        self.name = name
        self.identifier = identifier or name
        self.type = socket_type
        self.default_value = 0.0
        self.node = None


class _FakeSockets(list):
    def get(self, name):
        for item in self:
            if item.name == name:
                return item
        return None


class _FakeNode(object):
    # `node.type` in Blender is the RNA type name, not the bl_idname.
    RNA_TYPES = {
        "ShaderNodeBsdfPrincipled": "BSDF_PRINCIPLED",
        "ShaderNodeOutputMaterial": "OUTPUT_MATERIAL",
        "ShaderNodeTexImage": "TEX_IMAGE",
        "ShaderNodeNormalMap": "NORMAL_MAP",
        "ShaderNodeSeparateColor": "SEPARATE_COLOR",
        "ShaderNodeBump": "BUMP",
        "ShaderNodeMix": "MIX",
    }

    def __init__(self, node_type, name):
        self.type = self.RNA_TYPES.get(node_type, node_type)
        self.bl_idname = node_type
        self.name = name
        self.label = ""
        self.location = (0.0, 0.0)
        self.image = None
        self.interpolation = "Linear"
        self.extension = "EXTEND"
        self.space = "TANGENT"
        self.mode = "RGB"
        self.data_type = "RGBA"
        self.blend_type = "MIX"
        self.inputs = _FakeSockets()
        self.outputs = _FakeSockets()

        def add(collection, entries):
            for socket_name, socket_type in entries:
                collection.append(_FakeSocket(socket_name, None, socket_type))

        if node_type == "ShaderNodeBsdfPrincipled":
            add(self.inputs, [("Base Color", "RGBA"), ("Metallic", "VALUE"),
                              ("Roughness", "VALUE"), ("Normal", "VECTOR"),
                              ("Alpha", "VALUE")])
            add(self.outputs, [("BSDF", "SHADER")])
        elif node_type == "ShaderNodeOutputMaterial":
            add(self.inputs, [("Surface", "SHADER"), ("Displacement", "VECTOR")])
        elif node_type == "ShaderNodeTexImage":
            add(self.outputs, [("Color", "RGBA"), ("Alpha", "VALUE")])
        elif node_type == "ShaderNodeNormalMap":
            add(self.inputs, [("Strength", "VALUE"), ("Color", "RGBA")])
            add(self.outputs, [("Normal", "VECTOR")])
        elif node_type == "ShaderNodeSeparateColor":
            add(self.inputs, [("Color", "RGBA")])
            add(self.outputs, [("Red", "VALUE"), ("Green", "VALUE"), ("Blue", "VALUE")])
        elif node_type == "ShaderNodeBump":
            add(self.inputs, [("Strength", "VALUE"), ("Distance", "VALUE"),
                              ("Height", "VALUE"), ("Normal", "VECTOR")])
            add(self.outputs, [("Normal", "VECTOR")])
        elif node_type == "ShaderNodeMix":
            self.inputs.append(_FakeSocket("Factor", "Factor_Float", "VALUE"))
            self.inputs.append(_FakeSocket("A", "A_Color", "RGBA"))
            self.inputs.append(_FakeSocket("B", "B_Color", "RGBA"))
            self.outputs.append(_FakeSocket("Result", "Result_Color", "RGBA"))
        else:
            raise AssertionError("unexpected node type %s" % node_type)
        for socket in list(self.inputs) + list(self.outputs):
            socket.node = self


class _FakeNodes(object):
    def __init__(self):
        self.items = []

    def get(self, name):
        for item in self.items:
            if item.name == name:
                return item
        return None

    def new(self, node_type):
        node = _FakeNode(node_type, node_type)
        self.items.append(node)
        return node

    def remove(self, node):
        self.items.remove(node)

    def __iter__(self):
        return iter(self.items)


class _FakeLink(object):
    def __init__(self, from_socket, to_socket):
        self.from_socket = from_socket
        self.to_socket = to_socket


class _FakeLinks(list):
    def new(self, from_socket, to_socket):
        self.append(_FakeLink(from_socket, to_socket))

    def remove(self, link):
        list.remove(self, link)


class _FakeTree(object):
    def __init__(self):
        self.nodes = _FakeNodes()
        self.links = _FakeLinks()


class _FakeMaterial(object):
    def __init__(self, name):
        self.name = name
        self.use_nodes = False
        self.node_tree = _FakeTree()


class _FakeMaterials(object):
    def __init__(self):
        self.items = {}

    def get(self, name, default=None):
        return self.items.get(name, default)

    def new(self, name):
        material = _FakeMaterial(name)
        self.items[name] = material
        return material


def _link_from(tree, node, socket_name):
    """The output socket that feeds `node.<socket_name>`, or None."""
    target = node.inputs.get(socket_name)
    for link in tree.links:
        if link.to_socket is target:
            return link.from_socket
    return None


def _linked(tree, node, socket_name):
    """The node whose output feeds `node.<socket_name>`, or None."""
    socket = _link_from(tree, node, socket_name)
    return socket.node if socket is not None else None


def test_material_graph():
    print("material graph")
    import bpy

    bpy.data.materials = _FakeMaterials()
    images = {
        "normal": _FakeImage("NS_normal", 4, 4),
        "pbr_orm": _FakeImage("NS_pbr_orm", 4, 4),
        "pbr_base_color": _FakeImage("NS_pbr_base_color", 4, 4),
    }
    material = material_mod.ensure_material("Test")
    tree = material.node_tree
    applied = material_mod.sync(material, images, strength=0.75)
    check("normal wired", "normal" in applied)
    check("orm wired", "roughness" in applied and "metallic" in applied)
    check("base colour wired", "base_color" in applied)
    check("ao multiply wired", "ao" in applied)

    bsdf = compat.principled_node(tree)
    check("principled found", bsdf is not None)
    normal_source = _linked(tree, bsdf, "Normal")
    check("normal map node feeds the bsdf", normal_source is not None and normal_source.name == material_mod.NORMAL_MAP)
    if normal_source is not None:
        strength = normal_source.inputs.get("Strength")
        check("strength applied", abs(strength.default_value - 0.75) < 1e-6)
        normal_tex = _linked(tree, normal_source, "Color")
        check("normal texture feeds the map node", normal_tex is not None and normal_tex.image.name == "NS_normal")

    check("base colour goes through the ao mix", _linked(tree, bsdf, "Base Color").name == material_mod.AO_MIX)
    rough_source = _link_from(tree, bsdf, "Roughness")
    check(
        "roughness from the orm split",
        rough_source is not None
        and rough_source.node.name == material_mod.ORM_SPLIT
        and rough_source.identifier == "Green",
    )
    metal_source = _link_from(tree, bsdf, "Metallic")
    check(
        "metallic from the orm split",
        metal_source is not None
        and metal_source.node.name == material_mod.ORM_SPLIT
        and metal_source.identifier == "Blue",
    )

    # Re-running must not stack links.
    before = len(tree.links)
    material_mod.sync(material, images, strength=0.75)
    check("sync is idempotent", len(tree.links) == before, (before, len(tree.links)))
    check(
        "one principled bsdf",
        len([n for n in tree.nodes if n.type == "BSDF_PRINCIPLED"]) == 1,
    )

    # A height-only session gets a bump instead of a normal map.
    bpy.data.materials = _FakeMaterials()
    material = material_mod.ensure_material("Height only")
    tree = material.node_tree
    applied = material_mod.sync(material, {"height": _FakeImage("NS_height", 4, 4)}, strength=0.5)
    check("bump wired from height", "bump" in applied)
    bsdf = compat.principled_node(tree)
    check("bump feeds the bsdf", _linked(tree, bsdf, "Normal").name == material_mod.BUMP)

    # Without ORM the two scalar maps are used directly.
    bpy.data.materials = _FakeMaterials()
    material = material_mod.ensure_material("Separate")
    tree = material.node_tree
    applied = material_mod.sync(
        material,
        {
            "pbr_roughness": _FakeImage("NS_pbr_roughness", 4, 4),
            "pbr_metallic": _FakeImage("NS_pbr_metallic", 4, 4),
        },
        use_orm=False,
    )
    check("separate scalars wired", "roughness" in applied and "metallic" in applied)
    bsdf = compat.principled_node(tree)
    rough = _link_from(tree, bsdf, "Roughness")
    check(
        "roughness reads its own texture",
        rough is not None
        and rough.node.name == material_mod.ROUGH_TEX + " Value"
        and rough.identifier == "Green",
    )


# ---------------------------------------------------------------------------
# Material reconciliation
# ---------------------------------------------------------------------------


def test_material_reconciliation():
    print("material reconciliation")
    import bpy

    normal = _FakeImage("NS_normal", 4, 4)
    orm = _FakeImage("NS_pbr_orm", 4, 4)
    base = _FakeImage("NS_pbr_base_color", 4, 4)
    ao = _FakeImage("NS_pbr_ao", 4, 4)

    bpy.data.materials = _FakeMaterials()
    material = material_mod.ensure_material("Reconcile")
    tree = material.node_tree
    # A user node must survive every rebuild.
    user = tree.nodes.new("ShaderNodeTexImage")
    user.name = "User Tex"

    applied = material_mod.sync(
        material,
        {"normal": normal, "pbr_orm": orm, "pbr_base_color": base},
        strength=1.0,
    )
    check("first build wired", "normal" in applied and "ao" in applied)

    # The normal and ORM maps disappear: their nodes and links must go with
    # them, while the base colour and the user node stay.
    material_mod.sync(material, {"pbr_base_color": base})
    bsdf = compat.principled_node(tree)
    check("normal node pruned", tree.nodes.get(material_mod.NORMAL_TEX) is None)
    check("normal link cleared", _link_from(tree, bsdf, "Normal") is None)
    check("orm nodes pruned", tree.nodes.get(material_mod.ORM_TEX) is None)
    check("orm split pruned", tree.nodes.get(material_mod.ORM_SPLIT) is None)
    check(
        "base colour still wired",
        _linked(tree, bsdf, "Base Color").name == material_mod.BASE_TEX,
    )
    check("user node kept", tree.nodes.get("User Tex") is user)
    check(
        "one principled after reruns",
        len([n for n in tree.nodes if n.type == "BSDF_PRINCIPLED"]) == 1,
    )

    # A standalone AO map (no ORM) multiplies into base colour.
    bpy.data.materials = _FakeMaterials()
    material = material_mod.ensure_material("Standalone AO")
    tree = material.node_tree
    applied = material_mod.sync(material, {"pbr_base_color": base, "pbr_ao": ao})
    bsdf = compat.principled_node(tree)
    check("standalone ao wired", "ao" in applied)
    mix = _linked(tree, bsdf, "Base Color")
    check("ao mix feeds base colour", mix is not None and mix.name == material_mod.AO_MIX)
    if mix is not None:
        ao_source = _link_from(tree, mix, "B")
        check(
            "ao split feeds the mix",
            ao_source is not None and ao_source.node.name == material_mod.AO_SPLIT,
        )
        ao_tex = _linked(tree, ao_source.node, "Color")
        check(
            "ao image feeds the split",
            ao_tex is not None and ao_tex.image.name == "NS_pbr_ao",
        )

    # With Packed ORM on but no ORM image yet, separate maps still wire.
    bpy.data.materials = _FakeMaterials()
    material = material_mod.ensure_material("Fallback")
    tree = material.node_tree
    material_mod.sync(
        material,
        {
            "pbr_roughness": _FakeImage("NS_pbr_roughness", 4, 4),
            "pbr_metallic": _FakeImage("NS_pbr_metallic", 4, 4),
        },
        use_orm=True,
    )
    bsdf = compat.principled_node(tree)
    rough = _link_from(tree, bsdf, "Roughness")
    check(
        "missing orm falls back to the scalar maps",
        rough is not None and rough.node.name == material_mod.ROUGH_TEX + " Value",
    )

    # Releasing the images clears our graph but keeps the BSDF and user nodes.
    material_mod.clear(material)
    check(
        "clear removes managed nodes",
        all(not n.name.startswith(material_mod.PREFIX) for n in tree.nodes),
    )
    check("clear keeps the principled", compat.principled_node(tree) is not None)

    # A user's own link into a shaded input is not cut by an update.
    bpy.data.materials = _FakeMaterials()
    material = material_mod.ensure_material("User link")
    tree = material.node_tree
    bsdf = tree.nodes.new("ShaderNodeBsdfPrincipled")
    user_tex = tree.nodes.new("ShaderNodeTexImage")
    user_tex.name = "User Tex"
    tree.links.new(user_tex.outputs.get("Color"), bsdf.inputs.get("Base Color"))
    applied = material_mod.sync(material, {"pbr_base_color": base})
    check("user base colour link preserved", _linked(tree, bsdf, "Base Color") is user_tex)
    check("base colour reported unwired", "base_color" not in applied)
    check(
        "addon image node remains available",
        tree.nodes.get(material_mod.BASE_TEX) is not None,
    )

    # A same-named node of another type is a persistent conflict, not something
    # to overwrite silently.
    clash = _FakeTree()
    other = clash.nodes.new("ShaderNodeBump")
    other.name = material_mod.NORMAL_TEX
    try:
        compat.node(clash, material_mod.NORMAL_TEX, "ShaderNodeTexImage")
    except ValueError:
        check("node type collision reported", True)
    else:
        check("node type collision reported", False)
    check("colliding node untouched", other.type == "BUMP")


def test_material_signature():
    print("material signature")
    image = _FakeImage("NS_normal", 4, 4)
    other = _FakeImage("NS_pbr_base_color", 4, 4)
    one = material_mod.signature({"normal": image})
    check("signature is stable", one == material_mod.signature({"normal": image}))
    check(
        "signature tracks maps",
        one != material_mod.signature({"normal": image, "pbr_base_color": other}),
    )
    check(
        "signature tracks options",
        one != material_mod.signature({"normal": image}, strength=0.5),
    )


def test_material_assignment():
    print("material assignment")
    old = _FakeMaterial("Imported")
    old.use_nodes = True
    old_tree = old.node_tree
    slot = types.SimpleNamespace(material=old)
    obj = types.SimpleNamespace(
        type="MESH",
        data=types.SimpleNamespace(materials=[]),
        material_slots=[slot],
        active_material_index=0,
    )
    live = _FakeMaterial("N-Shade Live")
    check("assign returns true", material_mod.apply_to_object(obj, live))
    check("active slot points at the live material", slot.material is live)
    check(
        "old material datablock untouched",
        old.node_tree is old_tree and old.use_nodes is True,
    )

    empty = types.SimpleNamespace(
        type="MESH",
        data=types.SimpleNamespace(materials=[]),
        material_slots=[],
        active_material_index=0,
    )
    check("empty object gets the material", material_mod.apply_to_object(empty, live))
    check("slot appended", empty.data.materials == [live])


# ---------------------------------------------------------------------------
# Package registration
# ---------------------------------------------------------------------------


def test_package():
    print("package")
    import n_shade_bridge

    n_shade_bridge.register()
    check("register is idempotent enough to call", True)
    check("classes listed", len(n_shade_bridge.CLASSES) > 10)
    n_shade_bridge.unregister()
    check("unregister completed", True)


def test_ui_avoids_returning_none_calls():
    """`layout.label()` returns None, so a tooltip cannot be chained on it.

    This crashed the panel with "'NoneType' object has no attribute
    'on_hover_text'" in Blender 4.3, which is not reproducible without a UI, so
    the source is checked instead.
    """
    print("ui layout rules")
    import ast

    path = os.path.join(ROOT, "n_shade_bridge", "ui.py")
    with open(path, "r", encoding="utf-8") as handle:
        tree = ast.parse(handle.read())
    bad = []
    for node in ast.walk(tree):
        if not isinstance(node, ast.Call):
            continue
        if not isinstance(node.func, ast.Attribute):
            continue
        if node.func.attr not in ("on_hover_text", "on_disabled_hover_text"):
            continue
        inner = node.func.value
        if (
            isinstance(inner, ast.Call)
            and isinstance(inner.func, ast.Attribute)
            and inner.func.attr in ("label", "separator", "operator")
        ):
            bad.append((inner.lineno, inner.func.attr))
    # operator() does return a value, so only label/separator are wrong.
    bad = [entry for entry in bad if entry[1] != "operator"]
    check("no tooltip chained on a label", not bad, bad)


# ---------------------------------------------------------------------------
# Catalogue drift guard
# ---------------------------------------------------------------------------


def test_catalog_matches_host():
    print("catalog matches host")
    source_path = os.path.join(
        os.path.dirname(ROOT), "crates", "ns_app", "src", "bridge_maps.rs"
    )
    if not os.path.exists(source_path):
        print("  skip host source not present")
        return
    with open(source_path, "r", encoding="utf-8") as handle:
        source = handle.read()
    start = source.index("pub fn id(self)")
    end = source.index("pub fn label(self)")
    block = source[start:end]
    host_ids = [
        literal
        for literal in (
            part.split('"')[1] for part in block.split("=>")[1:] if '"' in part
        )
    ]
    check("host ids were parsed", len(host_ids) >= 10, host_ids)
    check(
        "addon catalog matches the host",
        host_ids == catalog.ids(),
        "%s vs %s" % (host_ids, catalog.ids()),
    )


def main():
    test_protocol()
    test_catalog()
    test_state()
    test_handlers()
    test_material()
    test_mix_sockets()
    test_pairing()
    test_package()
    test_ui_avoids_returning_none_calls()
    test_catalog_matches_host()
    test_material_graph()
    test_material_reconciliation()
    test_material_signature()
    test_material_assignment()
    test_wire_round_trip()
    test_pixel_upload()
    print()
    if FAILURES:
        print("%d failure(s): %s" % (len(FAILURES), ", ".join(FAILURES)))
        return 1
    print("all offline checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
