# Operations — Step 0 architecture notes

Audit for the Operations system described in [`OPERATIONS_README.md`](OPERATIONS_README.md).
Write-up only; no existing behaviour was changed to add the first increment.

## Stack

- Bevy **0.19.1**, `bevy_egui` **0.42.0** (egui **0.36.2**), `glam` vectors.
- The app is an immediate-mode egui application driven from Bevy systems; there is
  no ECS-based tool framework to plug a new `Plugin` into.

## Answers to the Step 0 table

| Question | Answer |
|---|---|
| Bevy version | 0.19.1 (`crates/ns_app/Cargo.toml`). |
| Shape storage | `ns_core::Document` inside the `DocumentRes` resource. Layers are a tree: `Layer { kind, visible, opacity, blend, id_color, vector_op, ... }`. Shapes are `LayerKind::Shape(ShapeLayer)`; groups are `LayerKind::Group { children }`. Other kinds (`Flat`, `Fill`, `PhotoHeight`, `HeightPaint`, `ImportedNormal`) do not carry vector geometry. |
| Shape/path type | The project's own `ns_core` types: `ShapeSource::{Path, Primitive, Raster}`, `Path { subpaths, fill_rule }`, `SubPath`, `Node`, `Primitive::{Rect, Ellipse, Polygon, Star}`. No second path library is used. |
| Height map | Evaluated in `ns_core` (CPU) and in the GPU SDF path (`crates/ns_app/src/gpu.rs`, `normal_studio.wgsl`). Shapes contribute `height_px`, `profile`, `invert`, `softness_px`, `smooth_px`, `edge_aa_px`. |
| Normal map | Generated from the evaluated height/bevel in `ns_core`; exports are written by `ui/actions.rs` / `ui/export.rs`. |
| Undo/redo | `EditorState` holds document snapshots. `checkpoint_document(&doc)` starts one undo entry; `undo` / `redo` restore the snapshot. |
| Tools/selection | `state.selected: Option<Vec<usize>>` (path from the root) plus `state.multi_selected: Vec<Vec<usize>>`. Canvas tools and drags live in `ui/canvas`. Operations only read the selection unless the user presses Apply. |
| UI stack | `bevy_egui`. Panels are modules under `crates/ns_app/src/ui`, composed from `card_frame()`. The right dock (Props / Mesh / Bake / Light / PBR) is a resizable egui `SidePanel` with a `ScrollArea`. |
| Save/load | `ns_core::Document` is serde-serialized. This increment adds no saved fields: applied operations produce ordinary layers, so old projects open unchanged. |
| Units / resolution | Shape coordinates are document pixels; `doc.size` is the output resolution. Exports resample in `ui/actions.rs`. |
| Tests | 240 unit tests (including UI-layer action tests). There is no golden-export suite yet; see "Follow-ups". |

## Existing behaviour that must keep working

Draw / edit / move / resize / delete shapes; layer selection and multi-selection;
boolean groups; masks and clipping; height painting; imports; save/load; undo/redo;
normal/height export; the Bake, Lightmap and PBR docks.

## First increment (this change)

Additive files only:

- `crates/ns_app/src/operations.rs` — operation metadata, stable ids, pure
  placement evaluation, and the copy/shape limits.
  - **Array**: linear/grid repeat with X/Y spacing and a per-row shift. Copies
    keep the original size (plain duplication along X and Y).
  - **Mirror**: left/right, top/bottom or both axes, with selection-edge,
    selection-center or document-center pivot and an optional gap.
  - **Pyramid**: stacks copies that scale and step once per level from the
    selection center, reproducing the old compounding-scale array as its own
    operation.
- `crates/ns_app/src/ui/operations.rs` — the **Operations** card in the
  Properties dock: per-layer operation stack, enable toggles, per-entry
  parameter controls, validation messages, copy/shape estimates and Apply.
  - Each layer owns a stack keyed by its `LayerId`. Stacks start empty, so
    selecting a vector layer never previews or applies anything by itself.
    Entries can be added (Array, Mirror, Pyramid), enabled/disabled, and
    removed; disabled entries keep their settings but contribute nothing.
  - Live preview: every layer's enabled entries are planned against that
    layer's own source (the selected layer keeps the full multi-selection,
    other layers preview themselves) and all placements are combined into one
    temporary document. The 2D canvas and the 3D material therefore keep
    showing a layer's active operations when another layer is selected; the
    project, undo stack, Layers dock and exports stay untouched. The preview is
    rebuilt only when a stack, the relevant selection or the document counters
    change, and is cleared when no layer has an active entry.
  - Apply makes one undo checkpoint, duplicates the originals once per planned
    placement, transforms the copies, selects them, and marks the document
    dirty. Raster layers are refused before the checkpoint, so nothing is
    half-applied. Applied entries are switched off afterwards; their settings
    stay in the stack for another run.
  - Copies are named by shape type or layer kind with zero-padded numbering
    (`Rect 001`, `Star 002`, `Group 001`, ...); every copy and nested child gets
    fresh UUIDs from `Layer::duplicate`.
- `crates/ns_app/src/app.rs` — stores the per-layer operation stacks in
  `EditorState` (not serialized).
- `docs/architecture-notes.md` and a copy of this guide in `docs/`.

## Deliberate adaptations to the guide

The user asked for these tools **in the Properties panel**, so the card sits in
that dock (below the selected layer's own card) instead of in a separate
searchable panel. The operation picker is registry-backed
(`OperationKind::ALL`) and can grow without new UI plumbing.

- **Typed parameters instead of a schema renderer.** With three operations and
  a per-entry parameter block, a `ParamSchema`/`ParamValues` layer would still
  be more code than the operations. Validation is centralised in `plan_entry`;
  the schema layer is queued for when parameters need shared serialization or
  searchable parameter metadata (see follow-ups).
- **No Bevy plugin / feature flag.** The app is not ECS-tool-based; operations are
  pure functions plus an immediate-mode card. Keeping them out of systems matches
  the guide's "logic outside systems" rule.
- **Preview quality.** The live preview evaluates through the normal pipeline
  with a temporary document. Fast/Final quality levels, debouncing and async
  preview evaluation remain follow-ups; parameter changes ride the existing
  evaluation throttle.
- **Transforms are scale + offset.** `ShapeSource::transformed` is the existing
  affine seam. Array copies are pure translations and Pyramid copies translate
  plus uniformly scale; neither rotates. Radial Array and rotation need a small
  core affine/rotation extension first. Stars are converted to editable paths
  for mirrored copies because their radii/rotation fields cannot encode a
  reflection.
- **Raster selections are refused.** `ShapeSource::Raster` has no transform
  representation, so the card disables Apply with an explanation rather than
  producing stacked copies.
- **Limits** follow section 12: warn above 2,000 copies or 2,000 shapes, refuse
  above 4,096 copies or 20,000 shapes.

## Follow-ups

1. Fast/Final preview quality and debouncing for very large documents.
2. Golden-export baseline + load-old-file test.
3. Radial Array (needs rotation support in `ShapeSource`).
4. Offset Path / Stroke to Shape (needs a path offset/stroke module; evaluate
   `kurbo` or `lyon` against the current dependencies first).
5. Height-surface operations: Emboss/Engrave, Panel/Groove, Vent/Slot, Rivet,
   Circular details, using the existing height/profile pipeline.
6. Parameter schema + searchable operations list once the operation set grows.
