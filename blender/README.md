# N-shade Bridge for Blender

A Blender add-on that puts the N-shade editor's maps inside a blend file and keeps them
live while you paint: normal, height, curvature, edge wear, edge detect and ID maps from
the layer stack, plus every map the single-image PBR pipeline produces, including a packed
ORM texture.

Two ways to get maps in, both switchable from either side:

* **Realtime streaming** - one map follows your brush at up to viewport speed.
* **On demand** - `Pull now`, `Get selected`, `Get all maps` in Blender, or the
  **Send to Blender** button in N-shade's export card (all maps or just the ticked ones).

And one way back: **Send mesh to N-shade** exports the selected mesh (UVs, normals,
positions) to the editor, so the 2D canvas and the UV overlay line up with what you are
texturing.

Nothing starts unless you turn the bridge on, and the listener binds `127.0.0.1` only.
Neither program writes into the other's data.

```
 N-shade (Rust, Bevy)                              Blender (Python add-on)
 ─────────────────────                             ─────────────────────────
 Document ── evaluate ── normal Grid<Vec3>          socket thread (no bpy)
 PBR store ─ Arc<StageOutput>                        │  frames in, frames out
      ▲                    │                         ▼
      │            encode RGBA8 / RGBA16        queue: newest frame wins
   edit_serial           │                         │
   publish_epoch         ▼                         ▼
      │         TcpListener ── frame ──────►  timer @20 Hz (main thread)
      └── 127.0.0.1    16-byte header + JSON      │
          + token      + raw payload              ▼
                                             NS_<map> image datablocks
                                                  │
                                             Principled BSDF
```

## Install

Targets **Blender 4.3 through 5.x**. Nothing in it compares version numbers: sockets and
nodes are found by type and by name-with-fallback, so a localised UI or a renamed socket
degrades gracefully instead of breaking the material.

**One folder, one source.** `blender/n_shade_bridge/` is the add-on; everything else is a
copy made from it:

| Path | What it is |
|---|---|
| `blender/n_shade_bridge/` | the source, the only copy you edit |
| `blender/n_shade_bridge.zip` | the same files packaged for Blender - install this |
| `dist/N-shade/blender/n_shade_bridge/` | byte-identical copy inside the portable build, so an extracted download needs no separate add-on |

`__pycache__` never ships: the add-on packager and the portable packager both exclude it,
and a stale `.pyc` for a module that no longer exists is exactly how an add-on starts
behaving like a previous version.

*Blender 4.2 and newer (extension)*

*Preferences → Get Extensions → Install from Disk*, pick `blender/n_shade_bridge.zip`.

*Updating*: install the new zip the same way. Blender replaces the module with the same
manifest id (`n_shade_bridge`) instead of leaving a second copy behind, but **restart
Blender**: reloading scripts does not re-import submodules, so the old socket thread would
keep running last week's code.

*As a legacy add-on*: pick the `n_shade_bridge` folder instead, via *Add-ons > Install
from Disk*. If a Blender build ever refuses the zip, install the folder - it is the same
content.

*Rebuilding the archive* after editing the sources:

```powershell
powershell -ExecutionPolicy Bypass -File tools/package_blender_addon.ps1
```

It overwrites `blender/n_shade_bridge.zip`, keeps the module folder at the archive root
(the layout Blender expects) and strips `__pycache__`.

## Turn the bridge on

In N-shade: **Settings > Blender Bridge > Serve maps to Blender**. The status line shows
the bound port and how many frames Blender has pulled; unticking the box closes the
listener and deletes the pairing file so a stale token cannot point at a dead port.

Or start with it on:

```powershell
n-shade.exe --bridge
n-shade.exe --bridge --bridge-port 47200   # optional
```

The host writes a pairing file on start:

| Platform | Path |
|---|---|
| Windows | `%LOCALAPPDATA%\n-shade\bridge.json` |
| Linux/macOS, `XDG_RUNTIME_DIR` set | `$XDG_RUNTIME_DIR/n-shade-bridge.json` |
| Linux/macOS otherwise | `~/.config/n-shade/bridge.json` |

```json
{ "protocol": 2, "app": "n-shade", "port": 47100, "token": "…32 hex…", "pid": 1234 }
```

The add-on re-reads that file on every reconnect attempt, so **start order does not
matter**: open Blender first and it pairs and connects the moment N-shade's bridge comes up.
A web page can scan loopback ports but cannot read a local file, so it cannot produce the
token.

## The panels

**N-shade** - connection and the live stream.

| Control | What it does |
|---|---|
| Connect / Disconnect | Opens or closes the loopback session |
| Auto-pair | Reads port + token from the pairing file by hand |
| Stream while editing | Realtime streaming on/off. Off: maps arrive only from pulls and pushes |
| Streaming map | Which map the stream follows |
| Live size | Longest side of a streamed frame: 512 / 1024 / 2048 / 4096 / Native |
| Pull now | One map at the pull size, immediately |
| Send mesh to N-shade | Exports the selected mesh (UVs, normals, positions) into the editor |
| Blender defaults | Live 1024, pull 2048, 16-bit height, packed ORM, AO in base colour, bump from height, and normal/height/curvature/edge wear ticked |

**N-shade Maps** - the catalogue and bulk transfers.

| Control | What it does |
|---|---|
| Pull size | Longest side used by pulls and pushes (default 2048) |
| 16-bit height | Height and displacement travel as RGBA16 |
| Tick boxes | Which maps `Get selected` fetches |
| Get selected | Request every ticked map at the pull size |
| Get all maps | Request every map the host says it can serve |
| All / None | Tick or untick the whole catalogue |

Only maps the host can actually serve are marked available: the catalogue arrives on
connect and again whenever the PBR pipeline finishes a stage.

**N-shade Material** - the node graph.

| Control | What it does |
|---|---|
| Material | Datablock to wire into; created on first use |
| Strength | Normal Map (and Bump) strength |
| Packed ORM | Drive Roughness/Metallic from one texture instead of two |
| Multiply AO into base colour | glTF-style occlusion, because the Principled BSDF has no AO input |
| Bump from height | Used only when no normal map has been received |
| Update material when maps change | Rebuild automatically as maps arrive and options change |
| Assign to active mesh | Put the live material in the active object's active slot; the previous datablock is left untouched |
| Update material / Release images | Wire once / delete the streamed images, their GPU buffers and the generated nodes |

## Material setup

Maps are wired into a Principled BSDF as they arrive. **Update material when maps
change** is on by default, but the graph is rebuilt only when the map set, an image
datablock or a material option changes - a streamed pixel update reuses the existing
nodes and links. Scenes saved with an older add-on keep their stored setting; press
**Blender defaults** once to enable automatic wiring there.

| Streamed map | Nodes |
|---|---|
| `normal`, `pbr_normal` | Image -> Normal Map -> Principled.Normal |
| `pbr_base_color` | Image (sRGB) -> [AO multiply] -> Principled.Base Color |
| `pbr_orm` | Image -> Separate Color: R -> AO multiply, G -> Roughness, B -> Metallic |
| `pbr_roughness`, `pbr_metallic` | Image -> Separate Color -> the scalar input; also used as the fallback while ORM has not arrived and when **Packed ORM** is off |
| `pbr_ao` | Image -> Separate Color -> multiply into base colour, used when no packed ORM supplies AO |
| `height`, `pbr_height`, `pbr_displacement` | Image -> Bump -> Principled.Normal, only when no normal map is wired |

The builder is non-destructive and idempotent:

- Every node it creates is named `NS ...`. Nodes outside that namespace are never
  renamed or deleted, and an existing node is never moved.
- Each update reconnects only its own links and removes `NS` nodes that no longer
  have a map - after **Release images**, or when a packed ORM map replaces separate
  roughness/metallic maps. No stale texture or second link is left behind.
- A link you made into Base Color, Normal, Roughness or Metallic from outside the
  `NS` namespace is left in place. The streamed map is not connected to that input;
  remove your link and press **Update material** to hand it over.
- Assigning the material swaps the active slot's material reference. The previous
  material datablock is not edited and stays in the file; Blender's normal orphan
  cleanup applies if nothing else uses it.

## How the streaming works

Per update, in order:

1. The timer (main thread, 20 Hz) decides what to send: a queued pull first, otherwise the
   live map, otherwise a `WATCH`.
2. `request_map()` encodes a ~110-byte frame into the worker's outbox. The main thread
   never touches the socket.
3. The worker `sendall`s it and blocks in `recv`. A reply is validated, its payload buffer
   allocated **once** at the exact final size and filled in place, then pushed into a
   one-slot queue (a newer frame replaces an unread one and bumps a "skipped" counter).
4. The next timer tick takes that frame, converts it with numpy, calls
   `Image.pixels.foreach_set` and `image.update()`.
5. The host answers `UNCHANGED` (a few dozen bytes) when neither the document nor the
   publish generation changed.

When a frame introduces a new map, replaces an image datablock (the 8/16-bit switch) or
the material options change, the add-on reconciles the node graph. Pixel updates alone
do not rebuild it.

**Live requests are long polls.** A `GET_MAP` carries `wait_ms`, and the host holds it until
the document's edit serial changes. A brush stroke therefore reaches Blender on the next
frame instead of at the next tick, and an idle session costs a few tiny frames per second
with no evaluation at all. The host caps evaluations at one at a time and ~16 per second.

**Pushes reach the add-on without polling.** `Send to Blender` in N-shade raises a publish
generation. Whichever reply is in flight when that happens - a `MAP` or an `UNCHANGED` -
carries the published map list, and the add-on enqueues them at the pull size. Manual mode
needs no polling to stay in sync.

The host never evaluates the document just because a client is attached: the cache key is
`(document pointer, edit serial, derived-map parameters, publish epoch, map, size, bits)`, so
a request that matches the last delivered frame is answered immediately. Evaluation runs on
Bevy's compute pool against a document snapshot, the same job protocol the editor uses for
its own background evaluation.

## Performance

The frame format is the important part. A map travels as raw RGBA8 (or RGBA16), which is
already the layout the host holds in its preview texture and the layout numpy wants, so no
encode/decode round trip exists on either side.

| Live size | Frame on the wire | Blender image | Conversion peak |
|---:|---:|---:|---:|
| 512 px | 1 MiB | 1 MiB | ~5 MiB |
| 1024 px | 4 MiB | 4 MiB | ~20 MiB |
| 2048 px | 16 MiB | 16 MiB | ~80 MiB |
| 4096 px | 64 MiB | 64 MiB | ~320 MiB |

16-bit height doubles the wire size and stores into a full-precision float buffer
(`use_half_precision = False`), because half floats would throw away the low bits the
transfer exists for: 2048 px of 16-bit height costs 64 MiB in Blender.

### Orientation

Blender stores pixels bottom-left first; the host sends rows top-down, the same order an
exported PNG uses. The add-on reverses the row order inside the existing conversion
ufunc, so images land the right way up and the flip costs no extra pass. If a map ever
looks mirrored vertically, the source is upside down, not the transfer.

### Disk

The frames themselves never touch the disk. The one thing that can grow a `.blend` is
that a generated image used by a material is serialised into the file, so a handful of 4K
maps would add a gigabyte to every save (and the same again to `.blend1`).
**Keep saves small** (on by default) drops every streamed buffer to a single texel in
`save_pre` and asks N-shade for the pixels again in `save_post`: the datablock and every
node link stay, the file stays small, and the viewport is back a fraction of a second
later. **Release images** deletes them outright.

What keeps it fast and bounded:

- **1024 px is the default live size.** It uploads in a few milliseconds, so nothing
  hitches while you paint; press `Pull now` at 2048/4096 when you want to look closely.
  A pull at full size pins that map, and the live stream will not downsample it afterwards.
- **One ufunc pass.** `np.multiply(flat, 1/255, dtype=np.float32)` converts and scales in
  a single pass over the buffer, half the memory traffic of `astype` followed by `multiply`.
- **One image per map, resized in place.** `image.scale()` instead of new datablocks, so
  node links survive and Blender never accumulates orphans (or saves them into the `.blend`).
- **One frame in flight.** The add-on never requests a second map before the first is
  answered, and the worker's queue holds one frame with newest-wins semantics.
- **No pixel reads.** `image.pixels[:]` would materialise four Python floats per texel; it
  is never used.
- **A separate 64 MiB evaluation-cache budget on the host** (`CACHE_BUDGET`). Larger
  documents stream instead of caching, so the bridge cannot double the editor's own cache.
- **A 4096 px cap on both sides**, so a `Native` request from an 8K document still produces
  a bounded frame.
- **Release images** hands everything back immediately.

## Blender safety

The rules the add-on is built around:

- **bpy is touched on the main thread only.** The socket thread imports no bpy and works
  exclusively with bytes and plain Python objects.
- **No blocking call on the main thread.** Even `sendall` happens on the worker; the UI only
  enqueues.
- **Timers never die silently.** A raising `bpy.app.timers` callback is unregistered by
  Blender, so the callback catches everything and always returns an interval.
- **File loads invalidate datablocks.** A `load_post` handler drops every cached image
  reference; keeping a handle to a freed ID is the classic addon crash.
- **Renders and bakes are respected.** With *Pause while rendering* on, the timer stops
  applying images while `RENDER` or `BAKE` owns the depsgraph. It keeps the link alive with
  pings, and the pending frame is applied afterwards.
- **No undo pollution.** Every operator is `{'REGISTER'}` only - connecting a socket is not a
  file edit and must not create an undo step.
- **Never hijacks a datablock.** An existing image that is not `GENERATED` is left alone; the
  add-on makes its own `NS_<map>` image. The material graph only edits nodes in its `NS `
  namespace, and assignment swaps the slot reference without editing the previous material.
- **Stale replies are ignored.** Every request carries a sequence number and a reply only
  clears the request it belongs to, so a frame the host finished after a newer request was
  issued cannot mark the newer one answered.
- **Protocol limits.** Header ≤ 8 KiB, payload ≤ 256 MiB, texels ≤ 4096², dimensions and
  format validated against the payload length before an Image is touched.
- **Generated images are not fake-users**, so if nothing references them a save drops them
  and the next pull recreates the buffer.

## Version support

| Blender | Status | Notes |
|---|---|---|
| 4.3 – 4.5 LTS | target | extension, `blender_manifest.toml` |
| 5.x | target | no 5.x-only API is used; unknown ops are ignored, not fatal |
| 3.6 – 4.2 | works, unsupported | the code paths are the same, the manifest is not |

The only differences that could bite are socket and node *names*, and those are handled by
lookup-with-fallback (`node.type`, `inputs.get(...)`, socket identifiers,
`colorspace_settings` candidates) rather than by comparing version numbers.

## Protocol

One frame:

```
magic "QXL1"  4 bytes
op            1 byte
flags         1 byte
hlen          2 bytes  u16 LE
plen          4 bytes  u32 LE
seq           4 bytes  u32 LE   echoed on the reply
header        hlen bytes of UTF-8 JSON
payload       plen bytes (raw RGBA8/RGBA16 for maps)
```

| Op | Direction | Purpose |
|---|---|---|
| `0x01 HELLO` | Blender → host | `{protocol, token, client, client_version, addon_version}` |
| `0x02 HELLO_OK` | host → Blender | `{app, app_version, session, max_size, epoch}` |
| `0x03 HELLO_ERR` | host → Blender | `{code, message}` — wrong token, protocol, busy, timeout |
| `0x10 GET_MAP` | Blender → host | `{map, max_size, bits, wait_ms, force, epoch}` |
| `0x11 MAP` | host → Blender | header + raw payload; `published` when a push happened mid-flight |
| `0x12 UNCHANGED` | host → Blender | `{serial, epoch}` — nothing to send |
| `0x13/0x14 PING/PONG` | both | keepalive during renders |
| `0x15 BYE` | both | orderly close |
| `0x16 WATCH` | Blender → host | `{epoch, wait_ms}` — notify me when the app pushes |
| `0x17 PUBLISHED` | host → Blender | `{maps, epoch, serial}` — the app pushed these |
| `0x18 CATALOG` | host → Blender | `{maps: [{id, label, group, space, bits, ready}]}` |
| `0x19 NOTICE` | host → Blender | `{level, message, map}` — non-fatal, shown in the panel |
| `0x1A PUSH_MESH` | Blender → host | `{path, name}` — the host imports the mesh file (OBJ/FBX/glTF/DAE/USD) |

`crates/ns_app/src/bridge.rs` and `n_shade_bridge/protocol.py` hold the two halves of this
contract. Change one, change the other. The map ids live in
`crates/ns_app/src/bridge_maps.rs` and `n_shade_bridge/catalog.py`; the offline test below
fails if the two lists drift apart.

## Tests

The add-on has no Blender in CI, so `blender/tests/test_offline.py` stubs the small part of
`bpy` it touches at import time and then exercises everything that is pure Python: the wire
codec (including a real socket round trip against a fake host), the catalogue and its drift
guard against the Rust source, the state machine, the request planner, the node-role
resolution, graph reconciliation (stale maps are pruned, standalone AO, ORM fallback, user
links and nodes survive), material assignment, and the pixel upload path with real numpy
(8-bit, 16-bit, resize, bit-depth switch, purge).

```powershell
python blender/tests/test_offline.py
```

## Troubleshooting

| Symptom | Cause |
|---|---|
| "No bridge.json" | The bridge is switched off in N-shade, or a different user profile |
| "token does not match" | Another N-shade instance wrote the file; press Auto-pair |
| Connects, no image | No map generated yet (PBR stages), or *Stream while editing* is off |
| Manual mode pulls nothing | The app must be on the same machine, bridge on, and a map generated |
| Slows down on 4K | Lower *Live size*; the add-on also caps the frame rate (see *Max frames per second*) |
| Image vanishes after save | Expected: *Keep saves small* cleared it. It returns on the next frame, or press Pull now |
| Mesh does not arrive | Select a mesh object first; the panel shows the host's reason in the notification line |
| Panel shows an old error | It clears on the next successful frame |

## Files

```
n_shade_bridge/
  __init__.py             bl_info, registration, atexit
  blender_manifest.toml   4.2+ extension metadata (min 4.3)
  version.py              version constant
  protocol.py             frame codec, limits (no bpy)
  client.py               socket thread, queues, long polls (no bpy in the thread)
  state.py                plain-data snapshot shared with the UI
  catalog.py              map ids/labels/groups; mirrors bridge_maps.rs
  compat.py               version/API probes, colour space, node helpers
  prefs.py                add-on preferences + safe accessor
  props.py                per-Scene settings
  apply.py                the only module that writes image pixels
  material.py             the reconciling node graph builder
  handlers.py             the main-thread timer and request planner
  ops.py                  connect/pair/pull/select/preset/material/release
  ui.py                   the three sidebar panels
  pairing.py              reads the host's bridge.json
```
