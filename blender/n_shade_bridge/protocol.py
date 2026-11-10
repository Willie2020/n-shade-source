"""Wire format shared by N-shade and the Blender addon.

One frame is::

    magic  4 bytes  b"QXL1"
    op     1 byte
    flags  1 byte
    hlen   2 bytes  u16 LE   header JSON length
    plen   4 bytes  u32 LE   payload length
    seq    4 bytes  u32 LE   echoed back on the reply
    header hlen bytes of UTF-8 JSON
    payload plen bytes of raw data

The payload is never base64 or JSON encoded: a map travels as raw RGBA8 or
RGBA16, which is exactly the layout the host already holds in memory and the
byte order numpy expects, so neither side pays for an encode/decode round trip.
`crates/ns_app/src/bridge.rs` mirrors these constants; change one side and the
other must change with it.

This module is pure Python (no bpy, no numpy) so the socket thread can import
it without touching Blender state.
"""

import json
import socket
import struct
import time

MAGIC = b"QXL1"
PROTOCOL_VERSION = 2

# magic | op | flags | hlen | plen | seq
HEADER = struct.Struct("<4sBBHII")
HEADER_SIZE = HEADER.size

# Session.
OP_HELLO = 0x01
OP_HELLO_OK = 0x02
OP_HELLO_ERR = 0x03
# Maps.
OP_GET_MAP = 0x10
OP_MAP = 0x11
OP_UNCHANGED = 0x12
OP_PING = 0x13
OP_PONG = 0x14
OP_BYE = 0x15
# Streaming control.
OP_WATCH = 0x16
OP_PUBLISHED = 0x17
OP_CATALOG = 0x18
OP_NOTICE = 0x19
# Blender -> host: a mesh file the host should import (UV layout, preview model).
OP_PUSH_MESH = 0x1A

OP_NAMES = {
    OP_HELLO: "HELLO",
    OP_HELLO_OK: "HELLO_OK",
    OP_HELLO_ERR: "HELLO_ERR",
    OP_GET_MAP: "GET_MAP",
    OP_MAP: "MAP",
    OP_UNCHANGED: "UNCHANGED",
    OP_PING: "PING",
    OP_PONG: "PONG",
    OP_BYE: "BYE",
    OP_WATCH: "WATCH",
    OP_PUBLISHED: "PUBLISHED",
    OP_CATALOG: "CATALOG",
    OP_NOTICE: "NOTICE",
    OP_PUSH_MESH: "PUSH_MESH",
}

# Hard ceilings. They exist so a malformed (or hostile) local frame cannot
# make Blender allocate an arbitrary buffer before any validation happens.
MAX_HEADER_BYTES = 8 * 1024
MAX_PAYLOAD_BYTES = 256 * 1024 * 1024
MAX_TEXELS = 4096 * 4096
MAX_PREVIEW_SIDE = 4096

# Timeouts, in seconds.
CONNECT_TIMEOUT = 3.0
HANDSHAKE_TIMEOUT = 5.0
# Short enough that a queued request goes out promptly (the worker flushes
# between reads) and long enough that waking up 20 times a second costs nothing.
IO_IDLE_TIMEOUT = 0.05
IO_STALL_TIMEOUT = 15.0

# Payload format names, mirrored by the host.
FORMAT_8 = "rgba8"
FORMAT_16 = "rgba16"


class ProtocolError(RuntimeError):
    """A frame was malformed, oversized, or not what the protocol expects."""


def encode(op, seq=0, header=None, payload=b""):
    """Serialize one frame into a single bytes object ready for sendall()."""
    head = b""
    if header:
        head = json.dumps(header, separators=(",", ":")).encode("utf-8")
    if len(head) > MAX_HEADER_BYTES:
        raise ProtocolError("header too large: %d bytes" % len(head))
    if len(payload) > MAX_PAYLOAD_BYTES:
        raise ProtocolError("payload too large: %d bytes" % len(payload))
    framing = HEADER.pack(
        MAGIC, op & 0xFF, 0, len(head), len(payload), seq & 0xFFFFFFFF
    )
    return framing + head + bytes(payload)


def _recv_exact(sock, view):
    """Fill `view` from the socket.

    Returns True when complete.  Returns False when the socket was idle before
    the first byte arrived and `IO_IDLE_TIMEOUT` elapsed, which lets the caller
    service its write queue.  Partial reads are never lost: the caller retries
    into the same view.
    """
    filled = 0
    total = len(view)
    deadline = time.monotonic() + IO_STALL_TIMEOUT
    while filled < total:
        try:
            got = sock.recv_into(memoryview(view)[filled:], total - filled)
        except socket.timeout:
            if filled == 0:
                return False
            if time.monotonic() > deadline:
                raise ProtocolError("stream stalled with %d/%d bytes" % (filled, total))
            continue
        if got == 0:
            raise ConnectionError("peer closed the connection")
        filled += got
    return True


def read_frame(sock):
    """Read one frame, or return None if the socket was idle.

    Memory behaviour: the payload buffer is allocated once, at its exact final
    size, and filled in place.  No chunk list, no repeated concatenation.
    """
    head = bytearray(HEADER_SIZE)
    if not _recv_exact(sock, head):
        return None
    magic, op, flags, hlen, plen, seq = HEADER.unpack(bytes(head))
    if magic != MAGIC:
        raise ProtocolError("bad magic %r" % (magic,))
    if hlen > MAX_HEADER_BYTES:
        raise ProtocolError("header length %d over limit" % hlen)
    if plen > MAX_PAYLOAD_BYTES:
        raise ProtocolError("payload length %d over limit" % plen)

    raw_header = bytearray(hlen)
    if hlen and not _recv_exact(sock, raw_header):
        raise ProtocolError("truncated header")
    try:
        header = json.loads(bytes(raw_header).decode("utf-8")) if hlen else {}
    except (UnicodeDecodeError, ValueError) as exc:
        raise ProtocolError("header is not valid JSON: %s" % exc)

    payload = b""
    if plen:
        block = bytearray(plen)
        if not _recv_exact(sock, block):
            raise ProtocolError("truncated payload")
        payload = block
    return op, flags, seq, header, payload


def bytes_per_texel(fmt):
    return 8 if fmt == FORMAT_16 else 4


def validate_map(header, payload):
    """Check a MAP frame before anything allocates an Image buffer.

    Returns (width, height, bits).
    """
    try:
        w = int(header["w"])
        h = int(header["h"])
    except (KeyError, TypeError, ValueError):
        raise ProtocolError("MAP frame without valid w/h")
    if w <= 0 or h <= 0:
        raise ProtocolError("MAP frame with empty size %dx%d" % (w, h))
    if w * h > MAX_TEXELS:
        raise ProtocolError("MAP frame %dx%d exceeds the texel ceiling" % (w, h))
    fmt = header.get("format") or FORMAT_8
    if fmt not in (FORMAT_8, FORMAT_16):
        raise ProtocolError("MAP frame uses unknown format %r" % (fmt,))
    bits = 16 if fmt == FORMAT_16 else 8
    expected = w * h * bytes_per_texel(fmt)
    if len(payload) != expected:
        raise ProtocolError(
            "MAP frame carries %d bytes, %dx%d %s needs %d"
            % (len(payload), w, h, fmt, expected)
        )
    return w, h, bits


def map_header(map_name, w, h, bits=8, **extra):
    header = {
        "map": map_name,
        "format": FORMAT_16 if bits == 16 else FORMAT_8,
        "w": w,
        "h": h,
        # Blender shades tangent-space normals with Y up, so the host always
        # converts from its internal OpenGL field; this is a statement, not a
        # knob.
        "space": "opengl",
    }
    header.update(extra)
    return header
