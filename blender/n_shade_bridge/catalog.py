"""The maps the host can serve.

The host announces its catalogue in `HELLO_OK`/`CATALOG`, and that is the list
the panel draws.  This module only carries the same ids, labels and groups so
the panel is useful before the first connection and so a stored selection can
be validated without a live session.  `crates/ns_app/src/bridge_maps.rs` is the
authority; keep the two lists in step.
"""

import collections

MapInfo = collections.namedtuple(
    "MapInfo", "id label group space bits ready"
)

DOCUMENT = "document"
PBR = "pbr"

_GROUP_LABELS = {DOCUMENT: "Document", PBR: "PBR"}

# id, label, group, colour space, suggested bits
_DEFAULTS = (
    ("normal", "Normal", DOCUMENT, "linear", 8),
    ("height", "Height", DOCUMENT, "linear", 16),
    ("curvature", "Curvature", DOCUMENT, "linear", 8),
    ("edge_wear", "Edge Wear", DOCUMENT, "linear", 8),
    ("edge_detect", "Edge Detect", DOCUMENT, "linear", 8),
    ("id_map", "ID Map", DOCUMENT, "linear", 8),
    ("pbr_normal", "Normal", PBR, "linear", 8),
    ("pbr_height", "Height", PBR, "linear", 16),
    ("pbr_ao", "AO", PBR, "linear", 8),
    ("pbr_cavity", "Cavity", PBR, "linear", 8),
    ("pbr_curvature", "Curvature", PBR, "linear", 8),
    ("pbr_edge_wear", "Edge Wear", PBR, "linear", 8),
    ("pbr_roughness", "Roughness", PBR, "linear", 8),
    ("pbr_metallic", "Metallic", PBR, "linear", 8),
    ("pbr_displacement", "Displacement", PBR, "linear", 16),
    ("pbr_base_color", "Base Colour", PBR, "srgb", 8),
    ("pbr_orm", "ORM (packed)", PBR, "linear", 8),
)


def defaults():
    """The catalogue as plain data, before any host has answered."""
    return [
        MapInfo(id, label, group, space, bits, False)
        for (id, label, group, space, bits) in _DEFAULTS
    ]


def group_label(group):
    return _GROUP_LABELS.get(group, group.title())


def ids():
    return [entry[0] for entry in _DEFAULTS]


def label_for(map_id):
    for (id, label, _group, _space, _bits) in _DEFAULTS:
        if id == map_id:
            return label
    return map_id


def parse(payload):
    """Turn a CATALOG/HELLO_OK map list into MapInfo tuples.

    Unknown fields are ignored and unknown ids are kept, so a newer host can
    add a map without this addon growing a release first.
    """
    entries = []
    seen = set()
    for item in payload or ():
        if not isinstance(item, dict):
            continue
        map_id = str(item.get("id") or "").strip()
        if not map_id or map_id in seen:
            continue
        seen.add(map_id)
        entries.append(
            MapInfo(
                map_id,
                str(item.get("label") or map_id),
                str(item.get("group") or DOCUMENT),
                str(item.get("space") or "linear"),
                int(item.get("bits") or 8),
                bool(item.get("ready")),
            )
        )
    return entries


def apply_readiness(catalog, ready_ids):
    """Same catalogue with the `ready` flag refreshed from a host list."""
    ready = set(ready_ids or ())
    return [info._replace(ready=info.id in ready) for info in catalog]


def split_selection(value):
    """`"normal, pbr_orm"` -> `["normal", "pbr_orm"]`."""
    if not value:
        return []
    return [part.strip() for part in str(value).split(",") if part.strip()]


def join_selection(values, catalog=None):
    """Join ids, dropping duplicates and keeping catalogue order when known."""
    wanted = set(values or ())
    if catalog:
        ordered = [info.id for info in catalog if info.id in wanted]
        ordered.extend(sorted(wanted - set(ordered)))
        return ",".join(ordered)
    return ",".join(sorted(wanted))
