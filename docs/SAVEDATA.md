# N-shade Save Data Structure

**Status:** implemented project format v2. The regular Save command writes the
portable ZIP archive described here. The loader still accepts v1 plain-RON
`.nsproj` files, and **Save legacy document (v1)...** writes one for older
builds. Autosave snapshots remain v1 RON.

## 1. Goals

A saved project must be **portable and reopenable on another PC** with the data
needed to continue editing and re-baking:

- the complete parametric normal-map document;
- the imported mesh data, including raw geometry, UVs, normals, index buffers,
  object/material groups, and UDIM metadata;
- the 3D viewer state for that mesh;
- the editor state that belongs to the project (selection, camera, brush,
  export and view settings);
- lightmap inputs and settings, including the HDR/EXR environment;
- custom fonts used by text layers.

The format must not store data that can be deterministically regenerated:
evaluated normal/height maps, AO/cavity/curvature, ID maps, lightmap output
images, UV overlay textures, previews, or caches.

## 2. Current format (v1)

A v1 `.nsproj` is a plain RON serialization of `ns_core::Document`, written by
`ns_core::io::save_project_ron`:

```rust
pub fn save_project_ron(path: impl AsRef<Path>, doc: &Document) -> Result<()> {
    let text = ron::ser::to_string_pretty(doc, ron::ser::PrettyConfig::default())?;
    write_atomic(path, text.as_bytes())
}
```

`Document` contains:

- document size, wrap mode, normal-map convention (`green`), and height scale;
- the layer tree and all source layer data;
- masks, mask settings, boolean operands, vector operations, cable metadata;
- raster/photo/imported-normal/painted-normal grids;
- UDIM tile tags on layers;
- `active_tile` is deliberately skipped during serialization.

It does **not** contain:

- imported mesh data or the mesh path;
- the 3D viewer transform or camera;
- editor state such as selection, brush settings, canvas view, or export flags;
- lightmap settings, scene lights, or the environment map;
- custom font bytes (only a file path is stored in `FontSource::File`);
- any derived map.

The v1 format is still accepted by the v2 loader for backward compatibility.

## 3. v2 at a glance

A v2 `.nsproj` is a single ZIP archive. The loader detects ZIP magic
(`PK\x03\x04`); a file that is not ZIP is treated as legacy v1 RON.

```text
my_project.nsproj                  ZIP archive, one portable file
├── manifest.json                  format metadata, asset table, hashes
├── project.ron                    project root: editor, viewer, meshes, lightmap
├── document.ron                   ns_core::Document (source layer stack)
├── meshes/
│   ├── viewer.nsmesh              normalized raw geometry for the 3D viewer
│   ├── lightmap-low.nsmesh        low-poly mesh used by the lightmap baker
│   ├── bake-high.nsmesh           optional high-poly source for high-to-low bakes
│   └── source/                    optional original files (not authoritative)
├── fonts/
│   └── <asset-id>-<name>.ttf      custom fonts used by text layers
└── environments/
    └── <asset-id>-<name>.hdr      HDR/EXR environments used by lightmaps
```

No `normal.png`, `height.png`, `ao.png`, `lightmap.png`, `preview.png`, or other
derived map is part of the archive. See §14.

| Data | Authoritative location |
|---|---|
| Parametric normal-map document, layer source data, masks, imported/painted/baked normal grids | `document.ron` |
| Mesh geometry, UVs, normals, primitive groups | `meshes/*.nsmesh` |
| UDIM tile metadata | Mesh entry in `project.ron`; layer `tile` tags in `document.ron`; active tile in `editor` |
| Editor state | `project.ron` → `editor` |
| 3D viewer state | `project.ron` → `viewer` and the referenced mesh |
| Lightmap inputs and settings | `project.ron` → `lightmap` and `environments/*` |
| Evaluated normal/height/AO/cavity/ID/lightmap outputs | **Not stored** — regenerated on demand |

## 4. Portable asset rules

1. All paths inside the archive use `/` separators and are relative to the
   archive root.
2. No absolute paths, drive letters, `..`, symlinks, or references outside the
   archive are allowed in authoritative metadata.
3. Every required asset is embedded in the archive. Optional external source
   files may be recorded for round-tripping, but they are not required to open
   the project.
4. The manifest records a BLAKE3 hash and byte size for every embedded asset.
   The loader verifies them by default.
5. Built-in fonts are compiled into the application and need no asset.
6. Machine-specific preferences (GPU adapter, denoiser device, output folder)
   are stored as hints only. The loader must be able to fall back.
7. Forward slashes are used even on Windows. When a path is surfaced to the OS,
   convert it at the last moment.

### 4.1 Portability checklist

- Copy the single `.nsproj` archive. The mesh, custom fonts, environment, and
  document are inside it.
- Do not copy only `document.ron`; that is an internal file and does not contain
  the mesh, editor state, viewer state, or lightmap inputs.
- A normalized `.nsmesh` mesh is authoritative. The optional original
  `.glb`/`.obj`/`.fbx`/`.dae`/`.usd` file is not needed to open the project.
- Built-in fonts are supplied by the application. Custom fonts are embedded.
- The archive contains no derived maps; they are regenerated after opening.
- Machine-specific settings such as the GPU backend and denoiser device are
  hints. Opening on a different PC may fall back to the CPU or another denoiser.

## 5. `manifest.json`

The manifest is the only file the loader needs before reading the project.
Unknown fields are ignored for forward compatibility.

```json
{
  "format": "n-shade/project",
  "format_version": 2,
  "min_reader_version": 2,
  "app_version": "0.6.8",
  "created_utc": "2026-11-07T12:00:00Z",
  "modified_utc": "2026-11-07T12:00:00Z",
  "root": "project.ron",
  "assets": [
    {
      "id": "mesh-viewer",
      "kind": "mesh",
      "path": "meshes/viewer.nsmesh",
      "hash": "blake3:9f2c...",
      "bytes": 48213,
      "codec": "none"
    },
    {
      "id": "env-studio",
      "kind": "environment",
      "path": "environments/env-studio-studio.hdr",
      "hash": "blake3:1a7b...",
      "bytes": 67108864,
      "codec": "none"
    },
    {
      "id": "font-inter-custom",
      "kind": "font",
      "path": "fonts/font-inter-custom-Inter.ttf",
      "hash": "blake3:8c10...",
      "bytes": 312044,
      "codec": "none"
    }
  ]
}
```

Asset kinds:

| Kind | Meaning |
|---|---|
| `mesh` | A normalized `.nsmesh` geometry file. |
| `font` | A custom font file used by `FontSource::File`. |
| `environment` | An HDR/EXR/LDR environment used by the lightmap baker. |
| `source_mesh` | Optional original mesh file kept for round-tripping. Not authoritative. |

`codec` is `none` or `zstd`. ZIP already compresses, so `none` is the default;
`zstd` is reserved for very large geometry if needed.

`hash` uses the form `blake3:<lowercase hex>`. Hashes are computed over the
stored bytes, before ZIP compression.

## 6. `project.ron`

`project.ron` is the project root. It references assets by ID; the manifest holds
the paths and hashes.

```ron
(
    format_version: 2,
    app_version: "0.6.8",
    document: "document.ron",

    editor: (
        active_tile: 1001,
        selected_layers: ["e2c1c3e0-..."],
        collapsed_groups: ["a44f1f3e-..."],
        canvas: (
            zoom: 1.0,
            pan: [0.0, 0.0],
            tool: Select,
            show_height: false,
            show_canvas_cursor: true,
            show_uv_overlay_2d: false,
            uv_overlay_opacity: 1.0,
            show_mask_overlay: false,
            mask_overlay_opacity: 0.5,
            mask_view: Overlay,
        ),
        viewport: (
            tab: ThreeD,
            split_view: false,
            split_stacked: false,
            right_tab: Properties,
            show_layers_dock: true,
            show_right_dock: true,
        ),
        brush: (
            radius: 48.0,
            hardness: 0.55,
            flow: 0.35,
            opacity: 1.0,
            mode: Raise,
            angle_deg: 0.0,
            symmetry: Off,
            symmetry_radial_count: 6,
            symmetry_pivot: [0.5, 0.5],
            mask_white: false,
            flatten_target: 0.5,
            stroke_width: 8.0,
            lasso_line_width: 4.0,
            tips: [
                (
                    name: "Round",
                    alpha: None,
                    normal: None,
                ),
            ],
            selected_tip: 0,
        ),
        export: (
            normal: true,
            height: true,
            project: true,
            edge_wear: false,
            curvature: false,
            edge_detect: false,
            id_map: false,
            resolution: 0,
            sixteen_bit: true,
            all_tiles: true,
            relative_dir: "output",
        ),
    ),

    viewer: (
        mesh_id: "mesh-viewer",
        preview_shape: Imported,
        transform: (
            translation: [0.0, 0.0, 0.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0, 1.0, 1.0],
        ),
        auto_rotate: false,
        surface_rotation: [0.0, 0.0, 0.0, 1.0],
        camera: (yaw: 0.35, pitch: 0.15, distance: 3.0),
        tiling: 1.0,
        show_uv_overlay: false,
        show_light_gizmos: true,
        material_preset: "MatteGray",
        lighting_preset: "Studio",
        environment_preset: "Neutral",
        lightmap_preview: false,
    ),

    meshes: [
        (
            id: "mesh-viewer",
            roles: [Viewer, LightmapLow],
            asset: "mesh-viewer",
            source: None,
            source_format: "glb",
            uv_bottom_left: false,
            vertex_count: 192,
            triangle_count: 188,
            udim: (
                enabled: false,
                base_tile: 1001,
                tiles: [],
            ),
            groups: [
                (
                    name: "Body",
                    object: "Cube",
                    material: "Default",
                    first_index: 0,
                    index_count: 564,
                ),
            ],
        ),
    ],

    fonts: [],

    lightmap: (
        mesh_id: "mesh-viewer",
        settings: (
            resolution: 1024,
            backend: Auto,
            lighting: Full,
            samples: 128,
            bounces: 2,
            tile: 1001,
            group: None,
            padding: 8,
            denoiser: Builtin,
            oidn_device: Auto,
            denoise_passes: 4,
            exposure: 0.0,
            sun_enabled: true,
            sun_azimuth_deg: 35.0,
            sun_elevation_deg: 55.0,
            sun_size_deg: 1.5,
            sun_color: [1.0, 1.0, 1.0],
            sun_intensity: 1.0,
            lights: [],
            ambient_color: [0.55, 0.62, 0.75],
            ambient_intensity: 0.25,
            environment_strength: 1.0,
            environment_fill: 0.0,
            seed: 1,
            pipeline: V2,
            png_bit_depth: Eight,
            highlight_rolloff: 0.0,
            ao_max_distance: 0.0,
        ),
        environment: None,
        output: (
            format: Png,
            relative_dir: "output/lightmaps",
            file_stem: "{mesh}_lightmap",
        ),
    ),

    bake: None,
)
```

### 6.1 Top-level fields

| Field | Required | Meaning |
|---|---|---|
| `format_version` | yes | `2` for this layout. |
| `app_version` | yes | Informational; used in diagnostics. |
| `document` | yes | Archive-relative path to the RON document. |
| `editor` | yes | Project-scoped editor state (§10). |
| `viewer` | yes | 3D viewer state (§9). |
| `meshes` | yes | Mesh table (§8). May be empty for a pure 2D document. |
| `lightmap` | yes | Lightmap inputs and settings (§11). |
| `bake` | no | Optional high-to-low bake inputs (§11.4). |

## 7. `document.ron` — normal-map source data

`document.ron` is the existing `ns_core::Document` serialization. It is the
authoritative source for the normal map.

Stored:

- document `size`, `wrap`, `green` convention, `height_scale_px`;
- every layer and group with stable `LayerId` UUIDs;
- `LayerKind::Shape` parameters: primitive/path/text/cable source, bevel radius
  and height, profile, inversion, softness, smoothing, edge AA, fill/stroke
  style, stroke width, cut/divide parameters;
- `LayerKind::ImportedNormal` and `LayerKind::NormalPaint` normal grids;
- `LayerKind::PhotoHeight`, `LayerKind::HeightPaint`, fill values;
- `ShapeLayer::baked_normal` from group bakes;
- `ShapeSource::Raster` / `RasterRegion` grids;
- layer masks, mask settings, mask references, boolean operands, vector
  operations, blend modes, opacity, visibility, lock state, tile tags, ID colors;
- text content, alignment, tracking, line height, and font source;
- cable metadata (waypoints/path, width, height, caps).

Not stored:

- the evaluated composite normal map;
- the evaluated height field;
- AO, cavity, curvature, convexity, edge wear, edge detect, ID map;
- the UV overlay texture or any preview texture;
- caches, undo history, or GPU/CPU evaluation buffers.

**Normal data policy:** imported, painted, and baked normal grids are kept
because they are document content. They may have been edited after import or
bake, and they cannot always be regenerated. The *evaluated* normal map is not
stored. This distinction is intentional.

### 7.1 Normal data inventory

| Normal-related data | Stored? | Notes |
|---|---|---|
| `green` / DirectX-OpenGL convention | Yes | External encoding convention; internal data is canonical OpenGL. |
| Shape/path/text/cable height sources | Yes | The normal is derived from these at evaluation time. |
| `ImportedNormal` grids | Yes | Baked or imported source content; may have been edited. |
| `NormalPaint` grids | Yes | Painted normal content. |
| `ShapeLayer::baked_normal` | Yes | Group-bake content kept beside its raster region. |
| Masks and mask settings | Yes | They change how the normal layers composite. |
| Blend modes, opacity, boolean/vector ops | Yes | They change the evaluated normal. |
| Evaluated composite normal map | **No** | Regenerated from the layer stack. |
| Evaluated height/AO/cavity/curvature/ID maps | **No** | Regenerated by `ns_core::derived`. |

### 7.2 Custom fonts

`FontSource::Builtin` needs no asset. For `FontSource::File`, the document keeps
its existing path field and the project root records a portable mapping:

```ron
fonts: [
    (
        original_path: "C:/Users/artist/Fonts/Inter-Custom.ttf",
        asset: "font-0",
    ),
],
```

The font bytes are copied to `fonts/<asset>-<name>.<ext>`. On load, the app
materializes the embedded bytes into its cache and rewrites
`FontSource::File.path` to that cache path. If the asset is missing, the built-in
Inter face is used and a warning is emitted. Legacy v1 files with only `path`
still load; if the path is missing, the same fallback applies.

## 8. Mesh data

Mesh geometry is stored as a normalized binary `.nsmesh` asset, independent of
the original import format. The original `.glb`/`.obj`/`.fbx`/`.dae`/`.usd` file
is optional and is never required to open the project.

### 8.1 Mesh entry (`project.ron`)

```ron
(
    id: "mesh-low",
    roles: [Viewer, LightmapLow],
    asset: "mesh-low",
    source: Some("mesh-low-source"),
    source_format: "glb",
    source_complete: false,
    uv_bottom_left: false,
    vertex_count: 192,
    triangle_count: 188,
    udim: (
        enabled: true,
        base_tile: 1001,
        tiles: [1001, 1002],
    ),
    groups: [
        (
            name: "Body",
            object: "Body",
            material: "Metal",
            first_index: 0,
            index_count: 300,
        ),
    ],
)
```

Roles:

| Role | Used by |
|---|---|
| `Viewer` | The 3D viewport mesh. |
| `LightmapLow` | Low-poly mesh for the lightmap baker. |
| `LightmapHigh` | Reserved; lightmaps currently use one low-poly mesh. |
| `BakeLow` | Low-poly mesh for a high-to-low normal bake. |
| `BakeHigh` | High-poly source for a high-to-low normal bake. |

A single mesh asset may have several roles. The viewer, lightmap and bake
sections reference mesh IDs, not file paths.

`source` is optional. When present, it points to an embedded original file kept
for round-tripping; `source_complete` is true only when that file has no external
references such as sidecar `.bin` files or loose textures. The normalized
`.nsmesh` asset is still authoritative for viewing, UV overlays, bakes, and
lightmaps.

### 8.2 What the mesh stores

The `.nsmesh` asset stores **raw file-space geometry**, not the transformed
viewer pose:

- positions: `f32 × 3` per vertex;
- normals: `f32 × 3` per vertex (optional; generated if missing);
- UVs: `f32 × 2` per vertex (required for UV-mapped meshes and baking);
- indices: `u32` triangles;
- primitive groups: object name, material name, first index, index count;
- triangle-group mapping used by `BakeMesh::triangle_groups` (derived from
  primitive groups on load);
- no textures, no derived normals, no baked maps.

The original file may contain external references (OBJ + MTL + textures, GLTF +
`.bin` + images, FBX textures). Those are not authoritative. If the optional
original file is included, the manifest marks whether it is self-contained. A
non-self-contained source is informational only. If the viewer is later extended
to display the source material rather than the normal-map preview material, those
textures must also be embedded as source assets; they are not part of the normal
map or its derived data.

### 8.3 `.nsmesh` binary layout

All values are little-endian. The file is 4-byte aligned.

```text
Header
  magic           8 bytes   "NSMESH01"
  version         u16       1
  flags           u16       bit 0: normals, bit 1: UVs, bit 2: bottom-left UV origin
  vertex_count    u32
  index_count     u32
  group_count     u32
  string_bytes    u32
  reserved        u32

Vertex data
  positions       f32[vertex_count * 3]
  normals         f32[vertex_count * 3]        if flag bit 0
  uvs             f32[vertex_count * 2]        if flag bit 1

Indices
  indices         u32[index_count]

Groups
  groups          MeshGroup[group_count]

Strings
  strings         u8[string_bytes]             UTF-8, length-prefixed entries

MeshGroup
  first_index     u32
  index_count     u32
  name_offset     u32
  object_offset   u32
  material_offset u32
```

Validation rules:

- `index_count % 3 == 0`;
- every index is less than `vertex_count`;
- group ranges are within `index_count` and do not overlap unintentionally;
- all positions, normals and UVs are finite;
- normals are renormalized on load if their length is not approximately 1;
- UVs are kept in their original UDIM space (for example `1001.x` is valid);
- `uv_bottom_left` comes from the mesh entry, not from the geometry bytes.

### 8.4 UDIM data

UDIM information lives in three places:

1. **Mesh entry:** `udim.enabled`, `udim.base_tile`, `udim.tiles`, and
   `uv_bottom_left`.
2. **Document layers:** each `Layer.tile` tag records which tile a layer belongs
   to. These tags already survive v1 saves and must keep surviving in v2.
3. **Editor state:** `editor.active_tile` records the tile being viewed.

The UV layout image and the tile overlay texture are derived and are not stored.
They are rebuilt from the mesh UVs on load.

## 9. 3D viewer state

The viewer section restores the preview without requiring the original source
format:

```ron
viewer: (
    mesh_id: "mesh-viewer",
    preview_shape: Imported,   // Cube | RoundedCube | Plane | Cylinder | Imported
    transform: (
        translation: [0.0, 0.0, 0.0],
        rotation: [0.0, 0.0, 0.0, 1.0],
        scale: [1.0, 1.0, 1.0],
    ),
    auto_rotate: false,
    surface_rotation: [0.0, 0.0, 0.0, 1.0],
    camera: (yaw: 0.35, pitch: 0.15, distance: 3.0),
    tiling: 1.0,
    show_uv_overlay: false,
    show_light_gizmos: true,
    lighting_preset: "Studio",
    lightmap_preview: false,
)
```

Stored per viewer:

- which mesh asset is displayed;
- the normalized preview shape when no mesh is imported;
- the mesh transform/rotation used by the viewer;
- orbit camera yaw/pitch/distance;
- UV tiling;
- UV overlay visibility (the overlay image itself is regenerated);
- light gizmo visibility;
- lighting/environment preview preset;
- whether the live lightmap lighting preview is enabled.

Not stored: Bevy entities, handles, texture IDs, render meshes, cage geometry,
transient hover/drag state, or GPU resources.

## 10. Editor state

The editor section stores project-scoped, portable state. Selections use stable
`LayerId` UUIDs so reordering does not change them.

```ron
editor: (
    active_tile: 1001,
    selected_layers: ["..."],
    collapsed_groups: ["..."],
    canvas: (
        zoom: 1.0,
        pan: [0.0, 0.0],
        tool: Select,
        show_height: false,
        show_canvas_cursor: true,
        show_uv_overlay_2d: false,
        uv_overlay_opacity: 1.0,
        show_mask_overlay: false,
        mask_overlay_opacity: 0.5,
        mask_view: Overlay,
    ),
    viewport: (
        tab: ThreeD,
        split_view: false,
        split_stacked: false,
        right_tab: Properties,
        show_layers_dock: true,
        show_right_dock: true,
    ),
    brush: (
        radius: 48.0,
        hardness: 0.55,
        flow: 0.35,
        opacity: 1.0,
        mode: Raise,
        angle_deg: 0.0,
        symmetry: Off,
        symmetry_radial_count: 6,
        symmetry_pivot: [0.5, 0.5],
        mask_white: false,
        flatten_target: 0.5,
        stroke_width: 8.0,
        lasso_line_width: 4.0,
        tips: [
            (
                name: "Round",
                alpha: None,
                normal: None,
            ),
        ],
        selected_tip: 0,
    ),
    export: (
        normal: true,
        height: true,
        project: true,
        edge_wear: false,
        curvature: false,
        edge_detect: false,
        id_map: false,
        resolution: 0,
        sixteen_bit: true,
        all_tiles: true,
        relative_dir: "output",
    ),
)
```

### 10.1 Stored

- active UDIM tile;
- selected and multi-selected layer IDs;
- collapsed group IDs;
- canvas zoom, pan, active tool, height-view toggle, cursor visibility;
- 2D UV overlay visibility and opacity;
- mask overlay visibility/opacity/view mode;
- viewport tab, split view, split layout, right-panel tab, dock visibility;
- brush radius, hardness, flow, opacity, mode, angle, symmetry, pivot, stroke
  and lasso widths;
- custom brush tips (alpha/normal grids) and the selected tip index;
- export map selection, resolution, 16-bit flag, all-tiles flag, and a
  **relative** export directory.

### 10.2 Not stored

- undo/redo history and gesture state;
- clipboard contents;
- hover, drag, snap-session, and active-guide state;
- texture handles, egui IDs, Bevy entities, and GPU state;
- evaluation timers, dirty flags, serials, progress counters, and status text;
- file-dialog folders and absolute paths;
- autosave configuration (application preference, not project data);
- PBR-panel input image and settings (separate tool, separate inputs);
- transient operation previews;
- per-layer operation stacks and their temporary preview documents (UI-only;
  they do not change the authoritative document);
- sci-fi generator parameters that have already been committed to layers (the
  resulting layers are stored, the transient generator UI state is not).

`relative_dir` is interpreted relative to the project file's folder. If it points
outside the project folder, the loader may accept it as a hint but must fall back
to `output/` rather than writing to an arbitrary absolute path.

## 11. Lightmap data

The lightmap section stores everything needed to reproduce a bake, but no baked
image.

```ron
lightmap: (
    mesh_id: "mesh-viewer",
    settings: (
        resolution: 1024,
        backend: Auto,
        lighting: Full,
        samples: 128,
        bounces: 2,
        tile: 1001,
        group: None,
        padding: 8,
        denoiser: Builtin,
        oidn_device: Auto,
        denoise_passes: 4,
        exposure: 0.0,
        sun_enabled: true,
        sun_azimuth_deg: 35.0,
        sun_elevation_deg: 55.0,
        sun_size_deg: 1.5,
        sun_color: [1.0, 1.0, 1.0],
        sun_intensity: 1.0,
        lights: [],
        ambient_color: [0.55, 0.62, 0.75],
        ambient_intensity: 0.25,
        environment_strength: 1.0,
        environment_fill: 0.0,
        seed: 1,
        pipeline: V2,
        png_bit_depth: Eight,
        highlight_rolloff: 0.0,
        ao_max_distance: 0.0,
    ),
    environment: None, // or "env-studio"
    output: (
        format: Png,
        relative_dir: "output/lightmaps",
        file_stem: "{mesh}_lightmap",
    ),
)
```

### 11.1 Stored inputs

- the low-poly mesh ID (mesh data is stored separately);
- `LightmapSettings`: resolution, backend preference, lighting mode, samples,
  bounces, tile, group, padding, denoiser, OIDN device, denoise passes, exposure,
  sun parameters, scene lights, ambient/fill, environment strength/fill, seed,
  pipeline, PNG bit depth, highlight rolloff, AO distance;
- the optional environment asset (HDR/EXR/LDR) embedded in `environments/`;
- output format, relative directory, and file-name pattern.

`uv_bottom_left` is taken from the referenced mesh entry and copied into the
lightmap settings at bake time. It is not stored twice.

### 11.2 Not stored

- the baked lightmap PNG/EXR or its preview texture;
- denoiser scratches, variance buffers, accumulation buffers, or GPU readbacks;
- bake progress, elapsed time, backend name, or ray statistics;
- per-tile batch job queues and status;
- the selected light gizmo / drag state.

The lightmap is regenerated from the stored mesh, environment, scene lights, and
settings. A missing environment is a warning; the bake can run with the solid
ambient fallback.

### 11.3 `bake` (optional high-to-low input)

If the project needs to reproduce a high-to-low normal bake, store its inputs:

```ron
bake: Some((
    low_mesh_id: "mesh-low",
    high_mesh_id: "mesh-high",
    settings: (
        mode: HighToLow,
        resolution: 1024,
        anti_alias: 1,
        space: TangentOpenGl,
        tile: 1001,
        cage_offset_pct: 1.0,
        ray_distance_pct: 5.0,
        bevel_radius_pct: 2.0,
        bevel_falloff: 1.0,
        normal_intensity: 1.0,
        padding: 8.0,
        uv_bottom_left: false,
    ),
    output: (
        relative_dir: "output/bake",
        file_stem: "{mesh}_normal",
    ),
)),
```

The resulting normal map is not stored as a file. If the bake was applied to the
document, the result exists as a normal layer grid in `document.ron`; that is
document content and is covered by §7. The `bake` section only makes re-baking
possible.

## 12. Archive layout details

- ZIP entry names are UTF-8 and use `/`.
- `manifest.json` and `project.ron` are required.
- `document.ron` is required unless the manifest declares an empty document
  (not supported in v2).
- Asset files live under `meshes/`, `fonts/`, and `environments/`.
- Optional original mesh files live under `meshes/source/` and are marked
  `source_complete` in their mesh entry.
- No `previews/`, `thumbnails/`, or `cache/` directories are written by default.
- If a thumbnail is ever added, it must be clearly non-authoritative, excluded
  from the manifest's required assets, and regenerable. Under the strict
  "no derived data" rule, it is better to omit it.

## 13. Save algorithm

1. Snapshot the current `Document` (clone / `SharedDocument` revision).
2. Build `EditorSaveData` from `EditorState`, converting selection paths to
   stable layer IDs and absolute export paths to a relative export directory.
3. Build `ViewerSaveData` from the current camera, preview shape, transform, and
   overlay flags.
4. Build `MeshEntry` records from:
   - the active viewer mesh;
   - the lightmap low-poly mesh;
   - optional high/low bake meshes.
5. Convert each raw mesh to `.nsmesh`; compute primitive groups, triangle counts,
   UDIM tiles, and `uv_bottom_left`. Do not apply the viewer transform to the
   geometry.
6. Copy custom font files into `fonts/` and record each original path in
   `project.ron`'s `fonts` table.
7. Copy the lightmap environment into `environments/` if present.
8. Write `document.ron` (compact RON) and `project.ron`.
9. Hash every asset; write `manifest.json`.
10. Zip to a temporary sibling file, flush/fsync, then atomically rename to the
    final `.nsproj` path.
11. Never include evaluated normal/height/AO/cavity/ID/lightmap outputs.

The save must be failure-atomic: a crash or full disk must leave the previous
project intact.

## 14. Derived-data policy

Do **not** store:

| Derived data | Why it is omitted |
|---|---|
| Evaluated normal map | Recomputed from the layer stack. |
| Evaluated height map | Recomputed from layer height sources. |
| AO / cavity / curvature / convexity | Recomputed by `ns_core::derived`. |
| Edge wear / edge detect | Recomputed from curvature. |
| Material ID map | Recomputed from layer ID colors. |
| UV overlay texture | Rebuilt from mesh UVs. |
| 3D preview texture / material | Rebuilt by the viewport. |
| Lightmap PNG/EXR output | Recomputed from mesh + lights + environment + settings. |
| PBR maps | Separate procedural tool; not part of the normal-map project. |
| Thumbnails / previews | Regenerable; excluded by default. |
| EvalCache, GPU scene, BVH, denoiser buffers | Runtime caches. |
| Undo/redo history | Session state, not document data. |

If a verification tool needs to compare output, it should render on demand and
compare against an external reference; it must not make the reference part of
the project archive.

## 15. Load algorithm

1. Detect ZIP magic.
   - ZIP: read `manifest.json`, validate `format` and versions.
   - Not ZIP: parse as legacy v1 RON `Document`, wrap it in a v2 project with
     default editor/viewer/lightmap sections, and mark it as migrated.
2. Validate manifest:
   - required files exist;
   - asset paths are relative, normalized, and inside the archive;
   - hashes and byte sizes match;
   - no unknown required asset kinds.
3. Read `project.ron`, then `document.ron`.
4. Load meshes:
   - parse `.nsmesh` into `RawMesh`/`BakeMesh`;
   - rebuild the viewer `Mesh` and UV overlay;
   - restore primitive groups, triangle groups, and UDIM metadata.
5. Load the environment into an `EnvironmentMap` from archive bytes.
6. Apply editor state:
   - resolve selected/collapsed IDs; drop missing IDs with a warning;
   - clamp zoom, camera, brush values to valid ranges;
   - resolve custom fonts by asset ID.
7. Apply viewer state and rebuild the 3D preview.
8. Leave derived maps untouched; evaluate them lazily on first use.
9. Report warnings for optional missing assets; fail on required missing assets
   or hash mismatch unless the user explicitly overrides.

## 16. Versioning and migration

- v1: plain RON `Document`, no format field.
- v2: ZIP archive with `manifest.json` and `project.ron`.
- The loader always supports v1 and the current v2.
- `format_version` is the archive schema version. `min_reader_version` is the
  oldest reader allowed to open the file.
- New optional fields use `#[serde(default)]`; unknown fields are ignored.
- Adding a required field requires a format-version bump and a migration.
- Saving writes the newest supported version.
- A "compatibility export" may write a v1 `Document` RON for external tools;
  this is a separate command, not the main save.

### 16.1 Older builds

A v2 archive is not readable by pre-v2 builds because it is a ZIP container.
The File menu therefore also offers **Save legacy document (v1)...**, which
writes a plain-RON `.nsproj` containing only the document. This file opens in
older builds; mesh, editor, viewer and lightmap data are intentionally not
included. The regular Save command always writes the portable v2 archive.

Migration from v1:

1. Parse the RON `Document`.
2. Wrap it in `ProjectSaveData` with default editor, viewer, lightmap, and an
   empty mesh table.
3. Leave custom font paths as legacy references; warn if missing.
4. Do not invent mesh data; the user must re-import the mesh.
5. Save as v2 on the next save.

## 17. Validation and error handling

Fatal errors:

- unsupported `format_version` or `min_reader_version`;
- missing `manifest.json`, `project.ron`, or `document.ron`;
- path traversal, absolute paths, symlinks, or duplicate asset IDs;
- missing required mesh/font/environment asset;
- hash or byte-size mismatch (unless `--ignore-hashes`);
- corrupt `.nsmesh` header, index range, or group range.

Warnings and fallbacks:

- missing optional original mesh source: open using normalized geometry;
- missing custom font: fall back to built-in Inter;
- missing environment: bake with ambient fallback;
- missing mesh for the viewer: open the 2D document only, show the default
  preview shape;
- missing selected layer IDs: clear the selection;
- invalid camera/brush values: clamp to defaults;
- missing UDIM tile list: derive tiles from layer tags;
- `relative_dir` outside the project: use `output/`.

## 18. Security and limits

- Reject ZIP entries with absolute paths, `..`, drive letters, or symlinks.
- Do not extract to the project folder; use a temporary/cache directory.
- Limit entry count, total uncompressed size, and compression ratio to prevent
  ZIP bombs.
- Accept only known asset kinds and extensions.
- Never execute scripts, shaders, or binaries from the archive.
- Verify hashes before use.
- Keep the existing limits: mesh upload ceiling, 4096 bake resolution, layer
  count cap, and storage-buffer checks.
- Treat all archive bytes as untrusted input.

## 19. Implementation map

| Area | File |
|---|---|
| Project/editor/viewer/lightmap save structs | `crates/ns_core/src/project.rs` (new) |
| ZIP archive + manifest + hashes | `crates/ns_core/src/archive.rs` (new) |
| Legacy RON load/save | `crates/ns_core/src/io.rs` |
| `.nsmesh` encode/decode | `crates/ns_core/src/mesh_asset.rs` |
| Mesh import/restore and `.nsmesh` conversion | `crates/ns_app/src/mesh_io.rs` |
| Runtime save/load conversions | `crates/ns_app/src/project_data.rs` (new) |
| Editor/viewer/mesh/lightmap resource restore | `crates/ns_app/src/ui/actions.rs`, `ui/mod.rs` |
| Save/Open/Export UI | `crates/ns_app/src/ui/actions.rs`, `ui/top_bar.rs` |
| Autosave snapshots (still legacy v1 RON) | `crates/ns_app/src/autosave.rs` |
| CLI project read/write | `crates/ns_cli/src/main.rs` |
| Bake inputs and triangle groups | `crates/ns_bake/src/lib.rs` |

No change to `ns_bake` is required for lightmap settings; they are serialized
from `ns_bake::LightmapSettings` by the project layer.

## 20. Acceptance tests

1. **Round-trip:** save a document with shapes, masks, imported normals, custom
   font, UDIM-tagged layers, a mesh, viewer camera, editor selection, and
   lightmap settings; move the archive to a different folder and load it.
   Everything listed in this document must match.
2. **Portability:** no absolute paths or drive letters appear in
   `project.ron`/`document.ron`/`manifest.json`.
3. **No derived maps:** the archive contains no normal/height/AO/cavity/ID/
   lightmap output files.
4. **Legacy:** a v1 plain-RON `.nsproj` still opens.
5. **Missing assets:** removing a custom font, environment, or optional source
   mesh produces a warning and the documented fallback, not a crash.
6. **Hash mismatch:** a modified embedded asset fails by default and is reported
   clearly with `--ignore-hashes`.
7. **Mesh fidelity:** vertex/index counts, UVs, triangle groups, object/material
   names, UDIM tiles, and `uv_bottom_left` survive.
8. **UDIM:** layer tile tags, mesh tile list, and active tile survive; the UV
   overlay is rebuilt, not loaded.
9. **Viewer:** imported mesh, transform, camera, preview shape, auto-rotate,
   tiling, and overlay flags survive.
10. **Lightmap:** all settings and the environment survive; the output image is
    absent and can be regenerated.
11. **Atomic save:** interrupting a save leaves the previous `.nsproj` intact.
12. **Security:** path traversal, symlink, and ZIP-bomb fixtures are rejected.

## 21. Open decisions

These are intentionally left for implementation review:

1. **Extension:** keep `.nsproj` for the ZIP archive with legacy detection, or
   introduce `.nsz` and keep `.nsproj` as document-only. This document assumes
   `.nsproj` becomes the archive and remains backward-compatible.
2. **Grid storage:** v2 keeps `Document` grids inline in `document.ron` and
   relies on ZIP compression. If 4K raster/photo/normal layers make archives too
   large, a later version can externalize them as `grids/<id>.nsgrid` with a
   `GridRef` in the layer schema.
3. **Original mesh files:** optional and non-authoritative. Store them only when
   round-tripping to the source format matters.
4. **Undo history:** not stored. If session restore becomes a requirement, add a
   separate bounded history asset, not part of the authoritative project.
5. **PBR panel:** out of scope for this project format because it is a separate
   image-to-PBR workflow. It can get its own save file later.
