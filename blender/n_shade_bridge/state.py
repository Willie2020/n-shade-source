"""Connection state shared between the socket thread and the UI.

This module imports nothing from Blender on purpose.  The socket thread writes
a handful of plain attributes here (ints, strings, floats, tuples) and the
timer callback on the main thread reads them.  Attribute assignment under the
GIL is atomic, and every value is an immutable scalar, so no lock is needed for
what the UI displays.  Image buffers never live here; they travel through a
queue that the main thread drains, one frame at a time.
"""

import time

from . import catalog


class State(object):
    """Mutable snapshot of the bridge, safe to read from either thread."""

    def __init__(self):
        self.connected = False
        self.phase = "idle"  # idle | connecting | connected | error
        self.status = "Not connected"
        self.last_error = ""
        self.notice = ""
        self.session = ""
        self.app_version = ""
        self.port = 0
        self.max_size = 0
        self.epoch = 0
        # Catalogue announced by the host, or the built-in default before the
        # first handshake.
        self.catalog = catalog.defaults()
        self.published = []
        # Per-map delivery record: id -> dict(w, h, bits, serial, at, eval_ms).
        self.maps = {}
        # Counters.
        self.frames = 0
        self.bytes = 0
        self.dropped = 0
        self.requests = 0
        self.apply_ms = 0.0
        self.last_map = ""
        self.connected_at = 0.0
        self.last_frame_at = 0.0
        # Requests: `pending_seq` is the reply we are still waiting for.
        self.pending_seq = 0
        self.pending_kind = ""
        self.pending_since = 0.0
        self.last_request_at = 0.0
        self.latency_ms = 0.0

    # -- lifecycle --------------------------------------------------------

    def reset_session(self):
        self.connected = False
        self.session = ""
        self.app_version = ""
        self.pending_seq = 0
        self.pending_kind = ""
        self.pending_since = 0.0
        self.connected_at = 0.0

    def mark_connecting(self, port):
        self.phase = "connecting"
        self.status = "Connecting to 127.0.0.1:%d..." % port
        self.port = port
        self.last_error = ""
        self.notice = ""

    def mark_connected(self, session, app_version, epoch=0, max_size=0):
        self.connected = True
        self.phase = "connected"
        self.status = "Connected"
        self.session = session
        self.app_version = app_version
        self.epoch = int(epoch or 0)
        self.max_size = int(max_size or 0)
        self.connected_at = time.monotonic()
        self.last_error = ""
        self.notice = ""

    def mark_closed(self, reason):
        self.connected = False
        self.phase = "idle"
        self.status = "Disconnected: %s" % (reason or "closed")
        self.last_error = reason or ""
        self.session = ""
        self.pending_seq = 0
        self.pending_kind = ""
        self.connected_at = 0.0

    def mark_error(self, message):
        self.connected = False
        self.phase = "error"
        self.status = "Error: %s" % message
        self.last_error = message
        self.session = ""
        self.pending_seq = 0
        self.pending_kind = ""
        self.connected_at = 0.0

    # -- requests ---------------------------------------------------------

    def begin_request(self, seq, kind):
        self.pending_seq = int(seq)
        self.pending_kind = kind
        self.pending_since = time.monotonic()
        self.last_request_at = self.pending_since
        self.requests += 1

    def finish_request(self, seq=0):
        """Clear the in-flight marker, but only for the request it belongs to.

        A reply for an older request can still arrive after a newer one was
        issued (the host finishes an evaluation it already started); ignoring
        the sequence number would then mark the newer request as answered.
        """
        if seq and self.pending_seq and int(seq) != self.pending_seq:
            return False
        started = self.pending_since
        if started:
            self.latency_ms = (time.monotonic() - started) * 1000.0
        self.pending_seq = 0
        self.pending_kind = ""
        self.pending_since = 0.0
        return True

    def reset_request(self):
        self.pending_seq = 0
        self.pending_kind = ""
        self.pending_since = 0.0

    # -- data -------------------------------------------------------------

    def note_map(self, map_id, width, height, bits, header, elapsed_ms):
        self.maps[map_id] = {
            "w": int(width),
            "h": int(height),
            "bits": int(bits),
            "serial": int(header.get("serial") or 0),
            "epoch": int(header.get("epoch") or 0),
            "eval_ms": float(header.get("eval_ms") or 0.0),
            "at": time.monotonic(),
        }
        self.last_map = map_id
        self.apply_ms = float(elapsed_ms)
        self.last_frame_at = time.monotonic()
        # A frame that landed is the best evidence the link is healthy.
        self.last_error = ""
        self.frames += 1

    def note_catalog(self, entries):
        if entries:
            self.catalog = entries

    def note_error(self, message):
        """Report a problem without pretending the socket dropped."""
        self.status = "Error: %s" % message
        self.last_error = message
        self.notice = ""

    def note_notice(self, message):
        """A non-fatal host message. It must not masquerade as the status."""
        self.notice = message

    def uptime(self):
        if not self.connected_at:
            return 0.0
        return time.monotonic() - self.connected_at

    def info(self, map_id):
        return self.maps.get(map_id)

    def delivered(self, map_id):
        return map_id in self.maps


STATE = State()
