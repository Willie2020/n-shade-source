"""The socket thread.

Hard rule: **this module never calls into bpy from the worker thread.**  It
only moves bytes and pushes immutable events into queues; `handlers.py` drains
those queues on the main thread and is the only place that touches images.

The thread is also the only place that blocks.  `request_map()` (called from
Blender's main thread) just drops an encoded frame into a queue, so even a
stalled host can never freeze the UI.
"""

import queue
import socket
import threading
import time

from . import catalog, compat, protocol, state
from .version import VERSION_STRING


class BridgeClient(object):
    def __init__(self):
        self._lock = threading.Lock()
        self._thread = None
        self._sock = None
        self._stop = threading.Event()
        # Control messages are small and must not be lost; map frames are big
        # and only the newest one is worth anything.
        self._control = queue.Queue()
        self._maps = queue.Queue(maxsize=1)
        self._out = queue.Queue(maxsize=8)
        self._seq = 0
        self._connected = False

    # -- main thread ------------------------------------------------------

    @property
    def running(self):
        return self._thread is not None and self._thread.is_alive()

    @property
    def connected(self):
        return self._connected

    def start(self, host, port, token):
        if self.running:
            return False
        self._drain_queues()
        self._stop.clear()
        self._connected = False
        hello = {
            "protocol": protocol.PROTOCOL_VERSION,
            "token": token,
            "client": "blender",
            "client_version": compat.version_string(),
            "addon_version": VERSION_STRING,
        }
        state.STATE.mark_connecting(port)
        thread = threading.Thread(
            target=self._run,
            name="n-shade-bridge",
            args=(host, port, hello),
            daemon=True,
        )
        with self._lock:
            self._thread = thread
        thread.start()
        return True

    def stop(self, timeout=1.5):
        self._stop.set()
        with self._lock:
            sock = self._sock
        if sock is not None:
            # Shutting the socket down unblocks the worker's recv() so the
            # join below is bounded even if the host went silent.
            try:
                sock.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
        thread = self._thread
        if thread is not None and thread.is_alive():
            thread.join(timeout)
        self._thread = None
        self._connected = False

    def _next_seq(self):
        self._seq = (self._seq + 1) & 0xFFFFFFFF
        return self._seq

    def request_map(self, map_id, max_size, bits=8, wait=0.0, force=False):
        """Queue a GET_MAP. Returns the sequence number, or None."""
        if not self._connected:
            return None
        seq = self._next_seq()
        header = {
            "map": map_id,
            "max_size": int(max_size),
            "bits": int(bits),
            "wait_ms": max(0, int(wait * 1000.0)),
            "force": bool(force),
            # The generation we already know about. If the app pushes while
            # this request is in flight the reply carries the published list,
            # which is how a live session learns about a push of other maps.
            "epoch": int(state.STATE.epoch or 0),
        }
        try:
            frame = protocol.encode(protocol.OP_GET_MAP, seq, header)
        except protocol.ProtocolError as exc:
            self._push_control({"kind": "error", "message": str(exc)})
            return None
        self._push_out(frame)
        state.STATE.begin_request(seq, "map")
        return seq

    def watch(self, epoch, wait=1.0):
        """Queue a WATCH: the host answers when the app pushes something."""
        if not self._connected:
            return None
        seq = self._next_seq()
        header = {"epoch": int(epoch or 0), "wait_ms": max(0, int(wait * 1000.0))}
        try:
            frame = protocol.encode(protocol.OP_WATCH, seq, header)
        except protocol.ProtocolError as exc:
            self._push_control({"kind": "error", "message": str(exc)})
            return None
        self._push_out(frame)
        state.STATE.begin_request(seq, "watch")
        return seq

    def push_mesh(self, path, name=""):
        """Ask the host to import a mesh file (OBJ/FBX/glTF/DAE/USD)."""
        if not self._connected:
            return None
        seq = self._next_seq()
        header = {"path": str(path), "name": str(name or "blender mesh")}
        try:
            frame = protocol.encode(protocol.OP_PUSH_MESH, seq, header)
        except protocol.ProtocolError as exc:
            self._push_control({"kind": "error", "message": str(exc)})
            return None
        self._push_out(frame)
        state.STATE.begin_request(seq, "mesh")
        return seq

    def ping(self):
        if not self._connected:
            return False
        self._push_out(protocol.encode(protocol.OP_PING, 0, {"t": time.monotonic()}))
        return True

    def drain(self):
        """Pop everything the worker produced, newest map at most."""
        events = self.drain_control()
        try:
            events.append(self._maps.get_nowait())
        except queue.Empty:
            pass
        return events

    def drain_control(self):
        """Pop control messages and leave image frames queued."""
        events = []
        while True:
            try:
                events.append(self._control.get_nowait())
            except queue.Empty:
                return events

    # -- worker thread ----------------------------------------------------

    def _run(self, host, port, hello):
        reason = "closed"
        failed = False
        sock = None
        try:
            sock = socket.create_connection((host, port), timeout=protocol.CONNECT_TIMEOUT)
            sock.settimeout(protocol.IO_IDLE_TIMEOUT)
            try:
                sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
            except OSError:
                pass
            with self._lock:
                self._sock = sock
            sock.sendall(protocol.encode(protocol.OP_HELLO, 0, hello))

            reply = self._handshake(sock)
            if reply is None:
                reason = "handshake failed"
                failed = True
                return
            self._connected = True
            state.STATE.mark_connected(
                reply.get("session", ""),
                reply.get("app_version", ""),
                reply.get("epoch", 0),
                reply.get("max_size", 0),
            )
            self._push_control({"kind": "hello", "header": reply})

            while not self._stop.is_set():
                self._flush(sock)
                frame = protocol.read_frame(sock)
                if frame is None:
                    continue
                if not self._handle(sock, frame):
                    reason = "closed by host"
                    return
            reason = "stopped"
        except socket.timeout:
            reason = "the host stopped answering"
            failed = True
        except ConnectionError as exc:
            reason = str(exc) or "connection lost"
            failed = True
        except protocol.ProtocolError as exc:
            reason = "protocol error: %s" % exc
            failed = True
        except OSError as exc:
            reason = str(exc) or exc.__class__.__name__
            failed = True
        finally:
            self._connected = False
            with self._lock:
                self._sock = None
            if sock is not None:
                try:
                    sock.close()
                except OSError:
                    pass
            if self._stop.is_set():
                state.STATE.mark_closed("stopped")
            elif state.STATE.phase == "error":
                # A specific reason (token, handshake, bad frame) is already on
                # display; do not replace it with a generic one.
                pass
            elif failed:
                state.STATE.mark_error(reason)
            else:
                state.STATE.mark_closed(reason)
            self._push_control({"kind": "closed", "reason": reason})

    def _handshake(self, sock):
        deadline = time.monotonic() + protocol.HANDSHAKE_TIMEOUT
        while time.monotonic() < deadline and not self._stop.is_set():
            frame = protocol.read_frame(sock)
            if frame is None:
                continue
            op, _flags, _seq, header, _payload = frame
            if op == protocol.OP_HELLO_OK:
                return header
            if op == protocol.OP_HELLO_ERR:
                message = header.get("message") or "the host refused the connection"
                state.STATE.mark_error(message)
                self._push_control({"kind": "error", "message": message})
                return None
        if not self._stop.is_set():
            state.STATE.mark_error("the host did not answer the handshake")
            self._push_control({"kind": "error", "message": "handshake timeout"})
        return None

    def _handle(self, sock, frame):
        op, _flags, seq, header, payload = frame
        if op == protocol.OP_MAP:
            try:
                w, h, bits = protocol.validate_map(header, payload)
            except protocol.ProtocolError as exc:
                state.STATE.mark_error(str(exc))
                self._push_control({"kind": "error", "message": str(exc)})
                return False
            state.STATE.bytes += len(payload)
            state.STATE.finish_request(seq)
            self._note_publish(header)
            self._push_map(
                {
                    "kind": "map",
                    "map": header.get("map", ""),
                    "seq": seq,
                    "w": w,
                    "h": h,
                    "bits": bits,
                    "header": header,
                    "payload": payload,
                }
            )
            return True
        if op == protocol.OP_UNCHANGED:
            state.STATE.finish_request(seq)
            self._note_publish(header)
            self._push_control({"kind": "unchanged", "header": header})
            return True
        if op == protocol.OP_PUBLISHED:
            state.STATE.epoch = int(header.get("epoch") or state.STATE.epoch)
            published = [str(name) for name in header.get("maps") or ()]
            state.STATE.published = published
            state.STATE.finish_request(seq)
            self._push_control({"kind": "published", "maps": published, "header": header})
            return True
        if op == protocol.OP_CATALOG:
            entries = catalog.parse(header.get("maps"))
            state.STATE.note_catalog(entries)
            self._push_control({"kind": "catalog", "count": len(entries)})
            return True
        if op == protocol.OP_NOTICE:
            message = header.get("message") or "the host reported a problem"
            state.STATE.note_notice(message)
            self._push_control({"kind": "notice", "message": message})
            return True
        if op == protocol.OP_PONG:
            self._push_control({"kind": "pong"})
            return True
        if op == protocol.OP_HELLO_ERR:
            message = header.get("message") or "the host reported an error"
            state.STATE.mark_error(message)
            self._push_control({"kind": "error", "message": message})
            return False
        if op == protocol.OP_BYE:
            self._push_control({"kind": "closed", "reason": "the host shut down"})
            return False
        # Unknown ops are ignored on purpose: a newer host may send something
        # this addon does not know yet, and that is not a reason to disconnect.
        compat.log(10, "bridge: ignoring unknown op 0x%02x", op)
        return True

    def _note_publish(self, header):
        """Adopt the epoch of a reply and surface any publish it carried.

        The published list is read before the epoch is bumped, so the queue
        only receives maps the caller has not already fetched.
        """
        published = [str(name) for name in header.get("published") or ()]
        if published:
            state.STATE.published = published
            self._push_control({"kind": "published", "maps": published})
        epoch = header.get("epoch")
        if isinstance(epoch, int) and epoch > state.STATE.epoch:
            state.STATE.epoch = epoch

    def _flush(self, sock):
        for _ in range(8):
            try:
                frame = self._out.get_nowait()
            except queue.Empty:
                return
            sock.sendall(frame)

    # -- queues -----------------------------------------------------------

    def _push_control(self, event):
        self._control.put(event)

    def _push_map(self, event):
        try:
            self._maps.put_nowait(event)
        except queue.Full:
            # Latest wins. A queued 16 MiB frame the main thread has not
            # touched yet is worth less than the one arriving now.
            try:
                self._maps.get_nowait()
            except queue.Empty:
                pass
            try:
                self._maps.put_nowait(event)
            except queue.Full:
                state.STATE.dropped += 1
                return
            state.STATE.dropped += 1

    def _push_out(self, frame):
        try:
            self._out.put_nowait(frame)
        except queue.Full:
            try:
                self._out.get_nowait()
            except queue.Empty:
                pass
            try:
                self._out.put_nowait(frame)
            except queue.Full:
                pass

    def _drain_queues(self):
        for q in (self._control, self._maps, self._out):
            while True:
                try:
                    q.get_nowait()
                except queue.Empty:
                    break


CLIENT = BridgeClient()
