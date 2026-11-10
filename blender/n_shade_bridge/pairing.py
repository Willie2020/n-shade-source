"""Finds the bridge.json that the host writes when its bridge starts.

The file is the pairing mechanism.  A page in a browser cannot read local
files, so a web page that scans localhost ports still cannot produce a valid
token and gets rejected at the handshake.  Nothing here is a long-term secret:
it exists so two programs on one machine agree, not to defend against someone
who already runs code as the same user.
"""

import json
import os
import sys


def config_path():
    if sys.platform.startswith("win"):
        base = os.environ.get("LOCALAPPDATA")
        if base:
            return os.path.join(base, "n-shade", "bridge.json")
        return os.path.join(os.environ.get("TEMP", "."), "n-shade-bridge.json")
    runtime = os.environ.get("XDG_RUNTIME_DIR")
    if runtime:
        return os.path.join(runtime, "n-shade-bridge.json")
    return os.path.join(os.path.expanduser("~"), ".config", "n-shade", "bridge.json")


def read():
    path = config_path()
    try:
        with open(path, "r", encoding="utf-8") as handle:
            data = json.load(handle)
    except (OSError, ValueError):
        return None
    if not isinstance(data, dict):
        return None
    return data


def apply_to(prefs_object, protocol_version=2):
    """Copy port and token from bridge.json into the given preferences.

    Returns (ok, message).  Never raises: the operator shows the message.
    """
    if prefs_object is None:
        return False, "Preferences are unavailable"
    data = read()
    if data is None:
        return False, "No bridge.json at %s" % config_path()
    version = data.get("protocol")
    if version != protocol_version:
        return False, "bridge.json speaks protocol %r, this addon speaks %d" % (
            version,
            protocol_version,
        )
    token = data.get("token") or ""
    if not token:
        return False, "bridge.json carries no token"
    prefs_object.token = token
    try:
        prefs_object.port = int(data.get("port") or prefs_object.port)
    except (TypeError, ValueError):
        pass
    return True, "Paired on 127.0.0.1:%d" % prefs_object.port
