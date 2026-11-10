"""Addon version, kept in one place.

`__init__.py` also carries the literal in `bl_info` and `blender_manifest.toml`
carries it again; Blender needs those static, so bump all three together.
"""

VERSION = (2, 1, 0)
VERSION_STRING = ".".join(str(part) for part in VERSION)
