# N-shade Manual

How to use the features added in this round:

- **Vector booleans on layers** (Add / Subtract / Intersect / XOR / Divide)
- **Live text and fonts** (`T`): click to place, edit the content, choose one of three
  embedded faces or load a TTF/OTF/TTC file, then bevel, outline, combine and export real
  glyph geometry
- **Multi-selection** (needed for booleans and grouping)
- **Cut with a stroked line** (the knife)
- **Ruler tool + Shift snapping** while drawing
- **Smart alignment guides** that snap a drawing node or a dragged shape/duplicate to nearby
  vertices, edges, centers and canvas lines
- **Duplicate array** with precise X/Y spacing
- **Edge smoothing** (`Smooth (px)`) for rough or jagged bevels
- **Lasso cleanup** (rectangular, elliptical, polygon, freehand) that trims shape geometry with
  rebuilt bevels or erases pixels through masks
- **Material ID maps** with one custom color per layer or group, exported as a hard-edged
  `id.png` for Substance Painter
- **Tunable edge wear / edge detect** exports behind compact, collapsible parameter rows
- **OpenGL / DirectX normal export** chosen directly on the export card
- **Height source** choice: direct layer heights (clean plateaus) or the legacy integration from normals
- **Optional sci-fi texture generator** (`Tools` menu) for procedural panel textures
- **Live Blender bridge** through the add-on in `blender/n_shade_bridge`: stream one map
  while you paint, or push all/selected maps from the export card (Settings > Blender Bridge,
  or `--bridge`)
- **Partial cut, partial slash and clip divide** boolean operations, alongside Add, Subtract,
  Intersect, XOR and Divide
- **Sweep along a path** with tangent rotation: any editable vector can be switched to a
  **Path guide** and used as the spine for an array of other shapes

Everything here works on the vector shapes that generate the normal map. The shapes
themselves stay out of the way: the 2D canvas shows the normal result plus the usual
selection outline, so you evaluate the result, not the construction geometry.

---

## 1. Selecting several layers

Booleans, grouping and cutting all act on a selection, so this is the first thing to
know.

| Action | Result |
|---|---|
| Click a layer row | Select it (replaces the set) |
| **Ctrl/Cmd-click** a row | Add/remove that row from the set |
| **Shift-click** a row | Select the whole range up to it |
| **Right-click** a row | Opens its menu and keeps the current set |
| **Ctrl/Cmd-click** or **Shift-click** on the canvas | Adds/removes the shape under the pointer from the set |
| **Delete** / **Backspace** | Removes every selected layer, exactly like the row menu's **Delete** |

The Layers header shows **"N selected"** whenever more than one layer is picked, so a
multi-selection is never invisible. On the canvas, **every selected layer is outlined** and
the whole set shares one union bounding box with the eight resize handles and the round
rotation handle. A single group works the same way: its visible descendants define the box,
and every shape inside scales or rotates around that shared centre. Ctrl/Cmd additions keep
a quieter box of their own without extra handles. Right-clicking a row to open its menu
never drops the rest of the set.

**Delete** (or **Backspace**) removes the whole selection at once, and Ctrl/Cmd+Z brings
it back. The key is left for the tool that is using it: with a node tool active it deletes
selected anchors, while a pen draft or an open mask edit keeps it for that gesture.

### Reordering the stack

| Gesture | Result |
|---|---|
| **Drag** a layer row (thumbnail, name or empty space) | Moves that layer; a ghost follows the pointer |
| Drop on the **top half** of a row | Inserts the layer above that row |
| Drop on the **bottom half** of a row | Inserts the layer below that row |
| Drop on the **middle** of a group row | Files the layer into the group |
| Drop on the strip between rows, or in the empty space below the stack | Inserts there; the bottom strip also moves a layer out of its group |
| Footer **↑ / ↓** | Moves the selected layer one step up or down |

The target is previewed with an accent insertion line at the row edge, or with an accent
outline around the group that will receive the layer. Dragging a group into itself or its
own children is rejected, so the outline never appears for an invalid target.

### Locking a layer

Click the padlock in a row — it appears on hover and stays visible while locked — or use
the row's right-click menu (**Lock** / **Unlock**). Locking a group also protects
everything inside it.

| Still works while locked | Blocked while locked |
|---|---|
| Selecting, renaming, visibility, blend/opacity, reordering (drag or ↑/↓), copying, export | Moving, resizing and rotating on the canvas, arrow-key nudges, lasso trim/erase, painting and mask editing, **Delete**, and the geometry actions (Outline stroke, Bake to pixels, Cut shapes below, booleans, grouping, mask binding) |

Locked rows show an accent padlock and the status bar explains why a gesture was
refused. The lock is stored in the project and is covered by undo.

### Bake to pixels

Right-click a shape row and choose **Bake to pixels** to replace its editable geometry
with the pixels it already draws. The bevel, profile and normal result do not change;
the layer simply stops being a vector.

On a group the same command merges the whole group into **one** layer. The children are
composed exactly as the stack renders them, live booleans included, and the group is
replaced by a single raster layer that keeps the group's name, blend mode, opacity,
mask, clipping and material ID. The normal result does not change; the children, their
bevels and their editable geometry are gone, so unlock any locked child first if the
command reports a locked layer.

- The action applies to the **whole selection**: every Ctrl/Cmd-selected row is baked in
  one undo step, and the selection stays active afterwards.
- A selected **group** becomes one **Raster** row: the group's name, order, mask, blend
  and opacity stay, and its children are composed into that layer's pixels. Its movable
  bounds come from the visible relief, so hidden mask sources no longer make the frame
  cover the whole canvas. It still selects, snaps and moves like the shapes it replaced;
  its bevel and profile are fixed because the exact normals are stored as pixels.
- Baked shapes are labelled **Raster** and use the four-square pixel thumbnail. They still
  select, snap, move, resize and rotate on the canvas, but node tools ignore them and
  Properties shows *Baked pixels · not vector-editable* instead of the vector controls.
- A baked shape stores only its own bounding box, not a full-canvas grid. Baked pixels are
  precomputed once when the layer changes and then composited by the GPU; the status bar
  stays on **GPU** instead of falling back.
- Locked layers are skipped and counted in the status bar. A group with any locked child is
  left untouched until it is unlocked, because a partial merge would change the result.
  Per-edge bevel profiles cannot be baked; the action reports that instead of dropping them.

### Blend modes

The Properties **Blend mode** dropdown lists the normal-map blends that are mathematically
valid. Simple overlay modes (Overlay, Multiply, Linear) are not offered: they treat the
three channels independently and destroy the normal.

| Mode | What it does | Use it for |
|---|---|---|
| **Derivative** | Adds the height slopes: `normalize(slope(a) + slope(b))`. | Height-derived layers, where adding slopes is exact. Detail over a steep base is subdued. |
| **RNM** | Reoriented normal mapping: rotates the layer into the base's tangent frame. | Detail over a base. Keeps the most detail and is the recommended default. |
| **Whiteout** | `normalize(vec3(a.xy + b.xy, a.z * b.z))`. | Wrinkles and grain. Flat inputs stay neutral and detail holds up on steep bases. |
| **UDN** | `normalize(vec3(a.xy + b.xy, a.z))`: drops the layer's Z. | The cheapest detail blend. Detail is slightly softer on flat bases. |
| **Replace** | Fades between the two surfaces in slope space. | Replacing one surface with another; opacity is the crossfade amount. |

Opacity scales the layer's slope for Derivative, RNM, Whiteout and UDN, so a half-opacity
layer behaves like a half-height layer. For Replace it is the crossfade amount, and the
midpoint keeps the full height of both surfaces instead of flattening like a vector lerp.
Use **RNM** when the detail must follow a strongly tilted base; **Whiteout** or **UDN** are
cheaper and read well on mostly flat surfaces.

### Duplicate with Alt-drag

| Gesture | Result |
|---|---|
| **Alt-drag** a shape or group on the 2D canvas | Copies the selection and drags the copy; the originals stay put |
| **Alt-drag** a layer row | Duplicates that layer at the drop position |
| Row right-click → **Duplicate** | Duplicates the selection directly above the originals, no drag needed |
| Row right-click / **Edit → Duplicate array...** | Repeats the selection at a precise X/Y offset; count includes the original |

- Copies land **directly above their originals**, are named `Name copy`, and become the
  selection, so Ctrl/Cmd+Z undoes the whole duplicate-and-move as one step.
- A **group** is copied whole, with fresh ids for every child; a **painted mask**, its
  settings, blend, opacity and all bevel parameters come along as an independent copy.
- When a **clipping mask or boolean operand** is part of the copied set, the copy points
  at the copied source instead of the original. If the source was not part of the set,
  the copy keeps using the existing one.
- Alt-dragging a member of a multi-selection copies **the whole set**. Layers with no
  outline (flat, photo, painted height) still duplicate, they just have nothing to drag,
  so the copy stays where it lands in the stack.
- The layer row menu and the canvas both work on hidden mask sources too: duplicate them
  from the row menu to get a second mask shape without touching the original.
- **Duplicate array** repeats the selection at a fixed offset: set **Count** (total copies,
  including the original) and **Offset X/Y** in document pixels. Each copy is one step
  further than the previous one, groups are copied whole, and the whole array is one undo
  step. A zero offset is disabled. Flat/photo layers duplicate but have no outline to
  offset, so their copies stay in the stack.
- While the array is created, all copies become the selection so they can be moved, grouped
  or exported together.
- **Alt-drag with smart guides:** the copy snaps to nearby edges, centers, vertices and
  canvas lines. Hold **Ctrl/Cmd** during the drag to place it freely. With a ruler placed
  and **Lock dupes** on, the copy follows the ruler angle exactly instead.
- Hold **Shift** while dragging a duplicate (or any selected shape/group move) to lock it to
  the nearest 45° ray. A horizontal or vertical lock keeps the copy's centre exactly on the
  original's centre line; smart guides pause while Shift is held, so they cannot pull the copy
  a few pixels off that line. Release Shift to resume normal guide snapping. While
  held, Shift also overrides a placed ruler's **Lock dupes** angle.
- Dragging any member of a multi-selection moves the whole set by the same amount; the
  selection is kept, and the smart guides use the union bounds.
- The shared box's resize handles scale a group or multi-selection rigidly around the
  opposite edge/corner, and its rotation handle turns every descendant around the union
  centre. Groups and multi-selections are snapshotted per gesture, so a group and one of
  its children never receive the same transform twice.

### Mirror with source side, clip and merge

Open **Properties → Operations**, add **Mirror** and enable it. The entry reflects the whole
selection about the pivot and updates a live preview of the exact result.

- **Axis** is left/right, top/bottom or both. **Pivot** is the selection edge, the selection
  centre or the document centre. **Gap** pushes the reflection away from the pivot instead of
  always toward +X/+Y, so a gapped copy can never slide back across the plane.
- **Source** chooses the authoritative half. **Both sides** (default) keeps the original and
  adds a reflected copy. A direction such as **Left → right** or **Top → bottom** keeps only
  that half, removes the opposite half, and mirrors the kept half over it — the same
  symmetrize behaviour as Blender's mirror from the negative or positive axis. Use it when an
  asymmetric external edge should replace the other side instead of leaving a faint overlap.
  A source side with no geometry is refused, and the **Gap** direction follows the chosen
  source side rather than the shape centre. The pivot must cross the selection, so use
  **Selection center** or **Document center**.
- **Clip at plane** (on by default) bisects the original and the reflection at the pivot: the
  source keeps the half its centre occupies and the copy keeps the opposite half. The halves
  meet exactly at the plane, so a centre mirror no longer duplicates the shape on top of
  itself. Groups are clipped child by child and keep their hierarchy. Selecting a single
  **Source** side turns Clip on automatically and disables the checkbox, because the opposite
  half must be replaced.
- **Merge seam** (on by default) welds the two halves into one shape when their bounds meet
  within **Merge (px)**. The union is computed on the vector outlines, so the seam disappears
  instead of leaving two coincident edges. Merge is skipped when the gap is wider than the
  threshold or the union would be two disconnected islands; the halves then stay independent
  layers. A welded group is flattened to one shape that keeps the source's style, mask,
  opacity, blend and stack position.
- With Source set to **Both sides** and Clip off, a reflection that crosses the pivot
  overlaps the original; the card warns about it. Turn Clip on for a precise bisect or Merge
  on to union the overlap. A single source side always clips.
- The preview and **Apply** run the same clip/merge pass, and the whole stack is one undo step.

### Sweep a shape along a path guide

A vector path can be used two ways. By default it is a **normal shape** and paints like any
other vector. Set **Properties → Use as → Path guide** and it becomes authoring geometry: it
stops painting, the canvas draws it as a dashed spine with a start and a hollow end dot, and
it can drive a **Sweep**.

The quickest route is the Sweep entry itself:

1. Draw a path with the **Pen** tool.
2. With the path selected, open **Properties → Operations** and add **Sweep**. A plain vector
   path is switched to **Path guide** automatically so the entry has a spine, and the entry's
   **Path** is set to it. The role change is part of the same undo step.
3. Drag the shape or group to repeat from the Layers panel into a **Shapes** slot in the Sweep
   entry. The copies appear on the spine immediately. The original stays where it is.
4. Set **Slots** to the number of source shapes you need and fill each slot by dragging more
   rows from the Layers panel. Every filled slot is swept together on each copy. Click a
   filled slot to clear it; empty slots fall back to the current selection.

Other ways to fill the entry:

- Pick any other guide in the entry's **Path** list.
- Leave the slots empty and select the shapes instead; the selection is what gets repeated.
- Make the guide by hand with **Use as → Path guide** before adding Sweep. An open path is the
  usual spine; a closed loop wraps the sweep all the way around without stacking a copy on the
  start point.

- **Copies** sets how many copies sit on the path. By default the original stays where it
  is, matching the other operations; **Hide original** removes it from the preview and leaves
  it hidden after Apply, so only the swept copies remain.
- **Shapes / Slots** hold the source layers dragged from the Layers panel. Dropping a layer
  into a slot never moves it in the stack; the sweep only reads it. A path guide cannot be a
  source and is rejected by the slot.
- **Rotate to tangent** turns each copy so its local +X axis follows the path. **Angle** adds a
  fixed rotation on top; turn Rotate off to keep every copy upright.
- **Start** and **End** are fractions of the path's length, so a sweep can cover only part of
  the spine. Copies are spaced by arc length, not by curve node.
- Hiding a guide with the eye only stops its dashed overlay; the sweep keeps using it. Path
  guides are never copied by an operation and never join a boolean.
- **Hide original** is available on every operation entry, not only Sweep. It hides the source
  layer's contribution while the entry is active and leaves that layer hidden after **Apply**;
  the source row stays in the stack, so its eye restores it. The arrayed copies keep their full
  size and are never hidden with it.
- The card previews the sweep live, and **Apply stack** commits every copy as one undo step.

#### Shape the spine on the 3D model

After a Sweep exists, its path can be refined without leaving the 3D preview:

1. Click **Edit path on the model** in the Sweep entry, or the node icon immediately after the
   projection icon in the 3D viewport header. This also turns auto-rotate off so the surface
   stays still under the pointer.
2. The guide's anchors and Bezier handles are drawn on the imported mesh. Drag an anchor to
   move it, or drag a handle tip to bend the curve. Every drag is raycast against the same
   triangles the preview renders and snaps to the surface, so the node lands on the model, not
   on an arbitrary screen plane. **Alt**-drag a handle to break a smooth pair.
3. The readout in the Sweep card and under the viewport reports the exact document pixels and
   UV of the last hit. The 2D canvas and the live sweep preview update as you drag.

Only the move or handle offset changes: the swept sources are never scaled, so every copy keeps
its original size and simply follows the new spine. The whole drag is one undo step.

### Nudge with the arrow keys

| Key | Movement |
|---|---|
| **← / → / ↑ / ↓** | Move the selection by **1 document pixel** |
| **Shift + arrow** | Move it by **10 pixels** |
| Arrow keys with a node tool active | Move the **selected anchors** instead of the layer |

- A selected group moves all of its shapes; a multi-selection moves together.
- Holding a key repeats the nudge, and the whole hold is **one undo step** — a single
  Ctrl/Cmd+Z puts it back where it started. Separate presses are separate steps.
- The nudge is in document pixels, so 1 px is exactly one texel of the normal map
  regardless of the canvas zoom.
- Arrows are ignored while a text field or another focused panel control is active (click
  the canvas again), during a canvas drag, a pen draft, an open mask edit, and while the
  Pan tool is toggled on.

---

## 2. Vector booleans on layers

The boolean runs on the **vector outlines** before anything is rasterized, so the
result never shows a seam, a doubled bevel or a leftover internal edge. The pieces stay
editable: a boolean group is a live compound, not a baked result.

### Create one

1. Select **two or more sibling** shapes with the same parent (Ctrl/Cmd-click, Shift-click
   or Ctrl/Cmd-click on the canvas).
2. Open **Layers → `+` → Boolean → Add / Subtract / Intersect / XOR / Divide**.
   The same submenu is on any layer row's right-click menu.
3. The shapes move into a boolean group and Properties shows the operation buttons under
   Blend mode.

### What each operation does

| Operation | Result |
|---|---|
| **Add** | Merges the silhouettes into one outline |
| **Subtract** | Cuts the shapes above the bottom one out of it |
| **Intersect** | Keeps only the area every shape covers |
| **XOR** | Keeps the areas covered by an odd number of shapes (removes overlaps) |
| **Divide** | Splits the overlaps into separately beveled regions |

### Change or unwrap the result

Properties, right under Blend mode:

- The five operation buttons switch the group to that operation live.
- **Off** returns the group to ordinary stacking (the scene is untouched).
- **Bake to shape** (Add/Subtract/Intersect/XOR) replaces the group with **one** editable
  shape holding the combined outline.
- **Split into shapes** (Divide) replaces the group with one editable shape per region.
- **Bake cut to shapes** (Partial cut / Partial slash / Clip divide) removes the live cut
  and keeps the base plus every cutter as editable layers, using the per-layer cut blends
  so the pocket, groove or seam keeps its exact surface. A cut cannot collapse to one
  outline without losing its height detail.
- **Release shapes** ungroups and keeps every original shape as a layer again.

Layer rows for boolean groups carry a badge such as `ADD · 2 SHAPES`, so you can see the
operation without opening Properties (badges: `ADD`, `SUB`, `INT`, `XOR`, `DIV`).

### Rules and behaviour

- The **bottom-most visible shape** supplies the bevel style (radius, height, profile,
  softness, smooth) and is the shape Subtract cuts out of.
- **Hidden shapes drop out** of the operation but stay in the stack — hide one to carve
  with fewer operands.
- Reorder children to change which shape is the base; drag a shape out of the group to
  remove it from the operation.
- Group opacity, blend mode and mask apply to the finished result. Per-shape opacity,
  blend and painted masks do **not** change the boolean geometry.
- Boolean groups use the **CPU tree evaluator** (the GPU preview path does not implement
  vector booleans).
- **Divide is meant for up to eight visible shapes.** Above that the group falls back to
  one merged outline so evaluation stays bounded, and Properties warns you.
- A group with fewer than two visible shapes warns instead of silently producing nothing.

### Where booleans are *not*

The **Blend mode** dropdown no longer contains Boolean Union / Intersect / Difference.
Those were surface blends applied after rasterizing. Old projects that still carry one
show a *"Legacy surface boolean"* note in Properties and keep evaluating as before;
picking a new layer boolean migrates the group. The Boolean menu also offers the three cut
operations (**Pocket**, **Slash**, **Clip divide**), documented in section 16.

---

## 3. Cut with a stroked line (knife)

Draw a line, give it a stroke width, and use it as a knife: the band is subtracted from
everything below and each resulting piece becomes its own layer.

### Steps

1. Pick the **Pen** tool (vector group in the tool rail).
2. Click your points. Hold **Shift** to snap segments to 45° steps, or to the ruler
   angle if a ruler is placed (see section 4). A live preview follows the pointer.
3. Press **Enter** (or double-click) to finish the path **open**. It becomes a layer named
   **Cut line** with `Style = Outline stroke`.

   Enter always ends the path the way you drew it — it never adds a closing edge you did
   not place. To close a loop into a filled shape, **click the first node**, which shows a
   ring while you draw. Open and closed pen layers both evaluate on the **GPU**: a cut
   line is packed as its stroke band, exactly like the CPU coverage path, so a stack of
   pen layers does not drop to the slow CPU evaluator.
4. Set the kerf in Properties:
   - **Stroke (px)** — total width of the band the knife removes.
   - **Style** must stay **Outline stroke**; a filled cutter cuts with its own outline.

   The **new cut edges take the bevel of the layer being cut** (its radius, height,
   profile, softness and Smooth value), not the cutter's. Set those on the target first if
   you want a particular cut-edge look — the cutter's own radius/height only affect how
   the knife is drawn before it is consumed.
5. Run **Cut shapes below** from any of:
   - Properties → *Cut shapes* → **Cut shapes below**
   - the layer row's right-click menu
   - **Layers → `+` → Cut shapes below**
6. The status bar reports **"Cut into N pieces"** and every piece is selected.

### What happens to the stack

- The cutter is **consumed** (undo brings it back).
- Every **visible shape or shape group below** it that the band overlaps is cut.
- Each connected piece becomes its **own editable path layer**, named after the target
  (`Panel 1/2`, `Panel 2/2`, …), keeping the target's bevel style, opacity, blend mode,
  painted mask, clipping and Smooth value.
- Layers **above** the cutter and layers that do not overlap it are left alone.
- The gap where the band ran stays flat: no coverage, no bevel, no stray normal.

### How the band is built

An open line has no interior, so the stroke rasterizes the **expanded band** — round
joins and round caps — rather than the outline of the implicitly closed polygon. That
means:

- What you see drawn is exactly the geometry the knife subtracts.
- The band is built as one simple loop, so a sharp corner in the line cannot stall the
  boolean (a self-crossing expander used to make the cut return nothing).
- A filled shape (no `Outline stroke`) cuts with **its own outline** — handy for
  punching a logo-shaped hole instead of cutting a line.

---

## 4. Ruler tool and Shift snapping

The ruler is a persistent angle guide. It is never a layer and it survives tool changes.

### Place a ruler

1. Pick the **Ruler** tile in the tool rail (above the Pan tool).
2. **Drag** on the canvas: the press point becomes the anchor, the drag direction the
   angle. Hold **Shift** while dragging to land on 15° steps.
3. A short **click** moves the anchor without changing the angle.
4. **Right-click** clears the guide (or use **Clear** in the options bar).
5. Click the **Ruler tile again** — or press **Esc** while the Ruler tool is active — to put
   the tool **and** its guide away. The tile toggles like the Pan tile, so one click off,
   one click back on.

Switching to another tool (Pen, Select, …) keeps the guide, so you can draw against it and
come back; only the toggle, Esc, right-click or **Clear** remove it.

The guide draws across the whole canvas: dashed line, tick marks, anchor dot and a live
angle chip. It renders at full strength while a drawing tool is active and fades to a
quiet reference otherwise.

### Options bar

Above the 2D canvas, while the Ruler tool is active or while a ruler exists (including
Select, so the lock is reachable while duplicating):

| Control | Effect |
|---|---|
| **Angle** | Type or drag an exact angle in degrees |
| **0° / 45° / 90° / 135°** | Set the angle exactly; with no ruler yet, clicking one places a ruler in the document centre |
| **Lock dupes** | Alt-dragged copies move only along the ruler angle (on by default; turn it off for free smart-guide dragging) |
| **Center** | Moves the anchor to the middle of the document |
| **Clear** | Removes the guide |
| hint text | Reminds you that Shift snaps drawing to the angle and that Alt-drag duplicates can follow it |

### Snapping while drawing

- **Shift with no ruler** — the pending segment snaps to the nearest **45°** step.
- **Shift with a ruler** — the pending segment snaps to the **ruler's angle**, so
  scattered clicks collapse onto one straight line at that angle.
- The **live preview snaps too**, so the node lands where the rubber band points, and
  the *projected* distance is stored (move the pointer along the line to lengthen or
  shorten it).
- Shift-dragging out Bézier handles aligns the handle direction the same way while
  keeping its length.
- The very first node is never snapped (there is nothing to anchor to).
- Without Shift, **smart guides** take over: the node snaps to nearby vertices, axes and
  parallel/perpendicular edges on its own (see section 5).

### Duplicate along the ruler angle

With a ruler placed, **Alt-drag** a shape or group: the copy moves only along the ruler
angle when **Lock dupes** is on (the default). Sideways pointer movement is discarded, so
repeated manual duplications stay exactly on the line. Hold **Ctrl/Cmd** during the drag
to place one copy freely. This applies to duplicate drags, not normal moves; group and
multi-selection moves keep smart guides. Turn **Lock dupes** off to let copies snap to
edges/centers instead. For exact spacing along the angle, use
**Edit → Duplicate array...** with `Offset X = cos(angle) * distance` and
`Offset Y = sin(angle) * distance` (the angle in radians, document Y down).

### Typical knife workflow

1. Ruler tool → drag a 30° guide.
2. Pen tool → click a start point, hold **Shift**, click an end point (the segment lands
   exactly on the guide), press **Enter**.
3. Set the line's **Stroke (px)**, then **Cut shapes below**. The cut edges take the
   target's bevel — set its **Smooth (px)** first if the result looks ribbed.

### Tips

- Enter ends the path open whether or not Shift is held; Shift is only for snapping.
- Esc drops the pen draft; the ruler stays.
- The ruler is stored in document space, so zooming and panning keep it aligned.

---

## 5. Smart alignment guides

While the Pen places a node it reads the vertices and straight edges of every visible
shape — and of the path you are drawing — and pulls the node onto the nearest alignment.
The same solver now runs while you drag a shape or group, and while you **Alt-drag a
duplicate**, so the moved bounds snap to other shapes, the canvas and its own reference
lines. This is the Illustrator/Affinity behaviour that keeps an object parallel with an
edge or exactly on another shape's corner, without holding a key.

### What it snaps to

| Guide | Effect | Colour |
|---|---|---|
| **Vertex** | The node or dragged corner lands exactly on a corner, path node or line endpoint | cyan |
| **Vertical / Horizontal** | The node or box takes the X or Y of another vertex | magenta |
| **Edge** | A dragged box edge aligns with another shape's bounding-box edge | magenta |
| **Center** | A dragged box center aligns with another shape's center | magenta |
| **Canvas** | A box edge or center aligns with the canvas edge or midline | blue |
| **Parallel** | The pending segment lines up parallel to an existing edge | teal |
| **Perpendicular** | The pending segment lines up perpendicular to an existing edge | violet |

- **Vertices beat axes, axes beat parallel/perpendicular.** Within 8 screen pixels of a
  vertex the node always takes the vertex; otherwise the closest constraint wins.
- **Both axes combine**, so taking the X of one vertex and the Y of another lands the
  node on their crossing — even when no vertex sits there.
- The dashed guide runs the full width of the canvas, a marker sits on the vertex that
  supplied it, and for parallel/perpendicular the reference edge is highlighted.
- A readout beside the pointer names the constraint with the exact angle and the pending
  length, e.g. `Parallel 32.4° · 180 px`, so you can click as soon as the numbers are right.

### Turning it on and off

- **View → Smart guides** toggles the feature; it is on by default.
- **Ctrl/Cmd held during a drag** temporarily disables snapping for that gesture.
- **Shift overrides it** for fixed angles: 45° steps, or the ruler's angle when a ruler is
  placed.
- The first node of a path snaps to vertices of other shapes, but has no axis or
  parallel/perpendicular constraint yet: there is no previous node to anchor the pending
  segment to.
- Hidden layers never supply references, and groups are read through, so a shape inside a
  collapsed group still provides corners and edges.

### Tips

- Hover just short of a corner, check the readout, then click: the node is stored exactly
  on the corner with no sub-pixel drift.
- To cut at a specific angle across a panel, start the line anywhere and hover near an
  edge that already runs at the angle you want: the parallel guide locks the direction.
- Turn the feature off in the View menu when you are tracing a freeform line close to
  existing geometry and do not want to be pulled onto it.

---

## 6. Edge smoothing (`Smooth (px)`)

Smooths the look of edges and bevels without changing their shape. Use it for ribbed or
jagged bevels, fuzzy mask borders, or traced rasters with stepped outlines.

### Where

- **Properties → Shape · path → Smooth (px)** (directly under Softness), range 0–8 px,
  default 0.
- **Properties → Group shape properties → Smooth (px)** when a group is selected: applies
  to every shape below the group.
- Cut pieces, boolean results and stroke bands inherit the value from their source style.

### What it does

`Smooth` averages two things before the bevel is built:

1. the **outline** — the coverage is blurred before the distance transform, so jagged
   pixel or vector edges and noisy mask borders come out clean;
2. the **shading direction** — the bevel gradient is read from a smoothed copy of the
   distance field, which removes the fine ribbing diagonal edges get from an exact
   distance transform.

The slope used for the profile (`t`) still comes from the exact distance, so the bevel
keeps its **profile, width and silhouette**. That is what separates it from Softness:

| Control | Effect on the bevel |
|---|---|
| **Smooth (px)** | Evens out the outline and shading; keeps profile and width — use for jaggies and ribs |
| **Softness (px)** | Blurs the distance: the slope gets softer and the ramp wider — use for a soft, rounded look |

### Recommended values

| Situation | Start at |
|---|---|
| Diagonal or curved cut/bevel edges | 2–4 px |
| Traced raster outlines, painted mask borders | 2–6 px |
| Sharp, engineered edges | 0 px (default) |

### Notes

- Default **0** keeps existing layers and saved projects bit-identical to before.
- Values are per shape and saved with the layer (`Smooth` missing in older files loads as 0).
- Smoothed shapes are evaluated once on the **CPU** and uploaded as a precomputed
  normal/height texture; the GPU then composites them with the rest of the stack. This
  keeps the preview and the exported maps identical without moving the whole document
  onto the CPU evaluator. Large documents still take a moment to refresh after the
  smoothed layer itself changes.
- Height and normal come from the same smoothed coverage, so direct height and the normal map
  stay consistent (and the integrated height follows the normals).

---

## 7. Lasso tools: trim shapes or erase pixels

One lasso tile in the tool rail, five lassos behind it (rectangle, ellipse, polygon,
freehand and line), and two modes in the options bar above the canvas.

### Pick the mode

| Mode | Effect |
|---|---|
| **Trim shape** (default) | Subtracts the region from shape **outlines**, so the bevel is rebuilt along the new edge exactly as if the shape had been drawn that way. A forward drag cuts the region away; a backward drag divides the shape instead. |
| **Erase pixels** | The original behavior: hides the region through painted masks and leaves every vector untouched. Direction does not change this mode. |

The **Line Lasso** is a knife: drag a straight line across the selection and the band
cuts the shapes apart. Its **Cut width** control (1–64 px, default 4 px) sets how wide
the removed band is. In Erase pixels mode the same band is erased through the masks
instead of splitting the outlines.

- Non-shape layers (painted height, imported photo, imported normal) have no outline to
  rebuild, so they are always erased through their mask, even in Trim shape mode.
- **Drag direction:** left→right and top→bottom **cut** the region away; right→left and
  bottom→top **divide** the shape and keep both sides.
- A **vector-only group** is trimmed as one unit and becomes flat pieces (like the knife).
  A **mixed group** that also holds paint/photo layers is masked as a unit, so its children
  are never silently flattened away.
- Cleanup follows the **selection**: one layer, a selected group, or every layer in a
  multi-selection. With nothing selected — or while **Alt** is held — the whole stack is
  targeted instead.
- **Shift** keeps its old meaning: restore masks instead of hiding them. A geometric trim
  cannot be painted back — use **Ctrl/Cmd+Z** for that.
- **Esc** cancels a polygon lasso that is still being drawn.
- One drag is **one undo step**, even when it trims shapes and erases a paint layer in the
  same gesture.

### Pick the region type

- Click the **lasso tile** to run the last-used lasso.
- **Right-click** the tile for the flyout: **Rectangular Lasso**, **Elliptical Lasso**,
  **Polygon Lasso**, **Freehand Lasso**, **Line Lasso**. The active one is highlighted,
  and the next tile click runs it.

### Draw a region

| Lasso | Gesture |
|---|---|
| Rectangular | Drag a box. Its start and moving corners snap to the **selected layer's centre, edge midpoints and corners** first, then to the usual smart guides. |
| Elliptical | Drag a box; the ellipse fills it. Snaps like the rectangular lasso. |
| Polygon | Click points, then **Enter**, double-click, or click the first point to close. A held press also places a corner, so aiming slowly works. |
| Freehand | Drag a free loop (simplified before the boolean runs) |
| Line | Drag a straight cut. Smart guides snap the start and end to vertices, axes and parallel/perpendicular edges; **Shift** falls back to 45° or the ruler angle. Unlike the other lassos, **Shift does not restore masks** for the line. |

**Moving a region before it is applied:** hold **Space** and drag. The whole
marquee (box, ellipse, polygon points or cut line) moves with the pointer instead of
panning the canvas, exactly like a temporary move mode. A polygon keeps its
corners and waits for **Enter** or a closing click; a box or line applies when the button
is released. Releasing **Space** returns to drawing, and **Esc** still cancels the region.

### Cut or divide by drag direction

The direction you drag decides the gesture:

| Drag | Result |
|---|---|
| **Left → right**, **top → bottom** | **Cut** — the region is trimmed away |
| **Right → left**, **bottom → top** | **Divide** — the inside and the outside both survive |

The dominant axis decides a diagonal drag; a tie goes horizontal. Box, ellipse and line
lassos read the corner you release on, because the pulled shape shows which way it was
drawn. Freehand and polygon strokes usually end where they began, so they read the
direction the stroke started in. The marquee turns orange and a **Divide** chip follows the
pointer while a backward drag is active; a forward drag shows a white **Cut** chip.

Divide keeps both sides: the part inside the region and the part outside survive as
separate editable layers (`Name 1/2`, `Name 2/2`), each with the original bevel, opacity,
blend, painted mask and clipping.

- A region that **misses** a shape, or covers **all of it**, leaves that shape alone: a
  divide only happens when the outline is actually crossed.
- Only shape layers and vector-only groups can be divided; paint, photo and flat layers
  have no outline (the status bar says so).
- The direction rule applies in **Trim shape** mode. **Erase pixels**, Shift-restore and an
  open mask always erase or restore, whatever the drag direction.
- The mouse button does not matter: left- and right-drags follow the same rule, and
  right-clicking polygon corners still places them. Use **Esc** to cancel a draft.

### What each mode changes

**Trim shape** replaces each trimmed layer in place with its remaining pieces, named
`Name 1/2`, `Name 2/2`, … Each piece keeps the original bevel (radius, height, profile,
carve, softness, smooth), its style (trimmed **Outline stroke** shapes become filled
bands), opacity, blend, painted mask, mask settings and clipping. A shape fully covered by
the region disappears. The pieces are selected afterwards and the status bar reports the
split. The Line Lasso uses the same pieces and reports **Cut N shapes into M pieces**.

**Erase pixels** adds or updates a **painted mask** (Properties → *Mask · painted*), with
the region composited to black. Everything else stays:

- every shape is still a vector path or primitive with its own bevel, smooth and stroke;
- the masks can be painted further with the brush, feathered, inverted, removed with
  **Remove**, or bypassed with **Mask enabled**;
- the masks live with their layers, so saving the project keeps the cleanup.

### Tips

- Lasso a thin strip across a panel to split it into two beveled halves that still line up.
- In Trim shape mode the lasso cuts the selected layer, group or multi-selection by
  default; hold **Alt** when every visible shape under the region should change. Selecting
  a group edits it as one unit instead of pulling its children apart.
- On a **Wrap: Repeat** document the trim happens inside the canvas; the cut is not wrapped
  across the seam.

## 8. Group bevel editing

Selecting a group shows **Group shape properties**, which edits every shape below it at
once:

- **Style** (Fill / Outline stroke) and **Stroke (px)**
- **Radius (px)** (1–10 for individual layer shapes and group edits, so a shared
  bevel stays readable), **Height (px)**, **Softness (px)**, **Smooth (px)**. Existing
  projects with a larger radius keep their value until the radius is edited, then it
  is clamped to the layer/group range.
- **Carve (invert)**, **Profile** (with the custom curve editor), chamfer angle
- **Fill rule** for path shapes, and **Convert N primitives to editable paths**

Values that differ across the group show a **mixed** tag; moving a control applies the
new value to all descendant shapes and bumps their revision so caches refresh.

---

## 9. Export: height source, normal convention, material IDs and edge masks

### Height source (direct vs integrated)

With the **Height** chip enabled, the export card shows a **Height source** row:

- **Direct (layers)** — composites the layer stack's height fields. This is the clean
  choice for normal-map authoring: flat plateaus stay flat, equal-depth slots stay equal,
  and there are no integration halos. Overlaps may differ from the normal map until the
  additive-height pass lands.
- **Integrated (normals)** — the default and former behavior. Rebuilds height from the
  final normal field with Frankot–Chellappa integration. It matches the normal map, but can
  drift across flat areas, ring at chamfer corners, tick on thin seams and shift plateau
  levels.
- **Auto** — direct unless a visible imported-normal layer is present, because imported
  normals have no analytic height. Such documents still use the final-field integration
  until per-layer integration lands.

If the exported height has glowing corners, uneven margins or different-depth slots, pick
**Direct (layers)**, keep 16-bit and Document resolution, and export again. The CLI accepts
`--export-height=auto|direct|integrated`; the app's `--integrate-height` flag only changes
the 2D Height view, not the exported file.

### Normal map convention (OpenGL / DirectX)

With the **Normal** chip enabled, the export card shows a **Normal Y** row:

- **OpenGL (Y+)** — the default; green points up (Blender, Substance, Unity OpenGL).
- **DirectX (Y−)** — green is flipped (Unreal, 3ds Max, most DirectX pipelines).

Only `normal.png` changes: the editor always stores canonical OpenGL data, and the 3D
preview always shows the surface, so it does not flip. The choice is saved with the project
and is also used to decode a normal map you import. On the command line use
`--normal-directx` or `--normal-opengl` together with `--export`.

### Assigning ID colors

Select a layer or group and open **Properties → Material ID**:

- **Assign color** picks the next unused palette color; the swatch opens a color picker
  and **Clear** removes the color so the layer falls back to the one below it (or black).
- The color is saved in the project, copied by duplicate, and carried by knife pieces and
  baked boolean results.
- A plain group's color fills the group, but children with their own color override it.
  A live vector compound is one surface: only the group's color applies.

### What the exported map contains

Enable **ID map** in the export card. `id.png` is an 8-bit RGB map:

- Each texel takes the **topmost colored layer** whose coverage is at least 50 %.
- Geometry, painted masks, clipping masks and boolean outlines shape each ID exactly like
  the normal map, so two IDs meet on the bevel silhouette with **no blended edge**.
- Opacity, blend modes and smart masks are ignored: an ID names a material, not a blend
  strength. Fully hidden layers contribute nothing.
- The map is resized with **nearest neighbor** only; averaging would mix two IDs into a
  third color along every shared edge.
- Uncovered texels stay black. If no layer has a color, the export still succeeds and the
  status bar adds *no ID colors assigned*.

### Tuning edge wear and edge detect

Select the **Edge wear** or **Edge detect** chip and a compact summary row appears under the
chips (`thr 0.12 · soft 0.78 · str 1.00`). Click the chevron or the title to expand it:

| Control | Effect |
|---|---|
| **Threshold** | Curvature below this is flat. Raise it to keep wear off soft creases. |
| **Softness** | Width of the ramp above the threshold; smaller is a harder edge. |
| **Strength** | Output multiplier (0 = empty map, 1 = full). |
| **Reset** | Back to the defaults, which reproduce the maps from before this control existed. |

Edge wear only uses convex curvature; edge detect uses absolute curvature so ridges and
creases are treated the same. Both are still derived from the final normal field, so masks,
clipping and booleans are included.

---

## 10. Quick reference

| Task | Where |
|---|---|
| Add/remove from selection | Ctrl/Cmd-click a row; Ctrl/Cmd- or Shift-click a canvas shape |
| Select a range | Shift-click a row |
| Delete the selected layer(s) | **Delete** or **Backspace** |
| Group the selection | **Ctrl/Cmd+G** (or Edit → Group) |
| Copy / paste layers | **Ctrl/Cmd+C / Ctrl/Cmd+V** (or Edit → Copy/Paste) |
| Duplicate | **Alt-drag** on the canvas or a row grip; row menu → **Duplicate** |
| Duplicate in an array | Row menu / **Edit → Duplicate array...**; set count and X/Y offset |
| Move a multi-selection | Drag any selected member; the whole set moves and stays selected |
| Lock a duplicate to 45° | Hold **Shift** while dragging (overrides ruler Lock dupes) |
| Lock duplicates to the ruler | Ruler options bar → **Lock dupes** (on by default) |
| Disable guides during a drag | Hold **Ctrl/Cmd** |
| Nudge the selection | Arrow keys (**Shift** = 10 px) |
| Trim or erase a region | Lasso tile → drag/click a region; options bar → **Trim shape** / **Erase pixels** (**Shift** restores masks, **Alt** = whole stack; cleanup follows the selection) |
| Divide a shape into two pieces | Drag the lasso **backwards** over the shape — right→left or bottom→top; both sides stay as layers |
| Place live text | Press **T** (or click the **Text** rail tile), then click the canvas once |
| Edit text content | Properties → **Text → Content**; the field is focused after placement |
| Change the typeface | Properties → **Text → Font**; **Load font file...** adds a TTF/OTF/TTC |
| Set spacing and alignment | Properties → **Text → Size / Tracking / Line height / Align** |
| Outline lettering | Properties → **Shape → Style: Outline stroke**, then **Stroke (px)** |
| Combine a word with a shape | Group the text and shape, then choose a vector boolean op |
| Keep text editable through a cleanup | Use lasso **Erase pixels**, not **Trim shape** |
| Rotate text | Drag the rotation handle; rotation bakes the glyph outlines into a path |
| Cut a shape along a line | Right-click the lasso tile → **Line Lasso**, drag the cut (smart guides snap the ends, **Shift** = 45°/ruler); **Cut width** in the options bar |
| Other lasso shapes | Right-click the lasso tile |
| Boolean submenu | `Layers → + → Boolean`, row right-click → **Boolean** |
| Switch / unwrap a boolean | Properties, under Blend mode |
| Finish a pen path open (knife) | **Enter** or double-click |
| Close a pen path into a shape | Click the first node (ring) while drawing |
| Snap drawing to a fixed angle | Hold **Shift** (45° steps, or the ruler angle) |
| Align a node to a vertex or edge | Hover: the smart guide snaps automatically (see section 5) |
| Turn the smart guides off | View → **Smart guides** |
| Keep the pointer visible while streaming | View → **Visible cursor on 2D canvas** (drawn reticle; on by default) |
| Place / move / clear the ruler | Drag / click / right-click; click the tile again or press Esc to turn it off |
| Exact ruler angle | Ruler options bar → **Angle** |
| Cut the shapes below a line | Properties, row menu, or `+` menu → **Cut shapes below** |
| Smooth a rough bevel | Properties → **Smooth (px)** |
| Give a layer or group a material ID | Properties → **Material ID → Assign color** |
| Export the material ID map | Export card → **ID map** → `id.png` |
| Tune edge wear / edge detect | Select the chip, then expand its summary row |
| Export a DirectX normal map | Export card → **Normal Y → DirectX (Y−)** |
| Export clean layer heights | Export card → **Height source → Direct (layers)** |
| Undo / redo | Ctrl/Cmd+Z, Ctrl/Cmd+Shift+Z or Ctrl/Y |
| Pan with a touchpad | Two-finger scroll (Settings → **Input**) |
| Show height as depth in the 3D preview | Settings → **3D Preview → Height** (Relief or Occlusion) |
| Strengthen or flatten that height effect | Settings → **3D Preview → Height scale** |
| Smooth the 3D preview silhouette | Settings → **3D Preview → Tessellation** |
| Zoom with a touchpad | Pinch or Ctrl/Cmd+wheel |
| Use pen pressure while painting | Properties or the mask toolbar → **Pen** |
| Switch tools | **V** Select, **R** Rectangle, **O** Ellipse, **S** Star, **T** Text, **P** Pen, **L** Lasso, **B** Paint, **H** Pan |
| Switch right-panel tabs | **Ctrl/Cmd+1…5** (Properties, Mesh, Bake, Lightmap, PBR) |
| Export with current settings | **Ctrl/Cmd+E** |
| Show every shortcut | **?** or View → **Keyboard shortcuts...** |
| Undo a destructive action | Click **Undo** in the toast, or Ctrl/Cmd+Z |
| Preview the map in Blender | **Settings > Blender Bridge > Serve maps to Blender**, then the N-shade panel in Blender connects on its own |
| Drive a Blender material from the live map | N-shade panel in Blender → **Update material** (or **Send to Blender** in the export card) |

---

## 11. Troubleshooting

**"Cut shapes below" reports it needs a stroked line**
The cutter must be a shape or an outline-stroke line that overlaps visible layers below
it. Check `Style = Outline stroke`, a **Stroke (px)** above 0, and that at least one
target layer sits below it in the same parent group.

**Enter ended my path but I wanted a closed shape**
Enter (and double-click) always ends the path **open**; it no longer closes loops. To
close one, click the **first node** — a ring is drawn around it once a second node
exists. Undo the open line and redraw it, clicking the first node at the end.

**The lasso does not clean anything**
Cleanup follows the selection, so select a layer, a group, or several layers first —
a selected group is edited as one unit. With **nothing** selected, or with **Alt** held,
the whole stack is targeted instead. The document also needs layers that generate a
normal; an empty document has nothing to hide. A polygon lasso needs three points before
Enter closes it.

**Lasso cleanup looks permanent**
It is painted masks, not a bake. **Shift**-draw the same region to restore it, paint it back
with the brush in mask mode, or turn **Mask enabled** off on the layers in Properties.

**Arrow keys do nothing**
The arrow nudge is ignored while a text field or panel control has focus (click the canvas
first), while the Pan tool is toggled on, during a canvas drag, a pen draft or a mask edit,
and when a node tool is active without a selected anchor. Drag the layer instead of
nudging if the pointer gesture is already busy.

**A node jumps onto nearby geometry when I want it free**
That is the smart guides. Turn them off in **View → Smart guides**, or hold
**Ctrl/Cmd** during a shape drag (Shift only fixes pen angles).

**The guides do not appear**
The Pen must be active and at least one node already placed; the very first node only
snaps to vertices, because there is no pending segment to align yet. Guides also need a
visible shape or a drawn segment to reference.

**Undo history is shorter than 24 steps on a huge document**
Undo keeps up to 24 steps, but the undo and redo stacks together are capped at
**512 MiB**. A 4K document with several painted masks can hit that in a few steps, so
the oldest snapshots are dropped first. The most recent step always stays undoable;
delete unused painted masks to keep more history.

**Delete does not remove the selected layer**
The key belongs to whatever is active: a node tool deletes selected anchors instead, a
pen draft or an open mask edit keeps it for that gesture, and a focused text field (for
example the layer **Name**) swallows it. Switch to the Select tool and click the layer
row, then press Delete.

**Shift does not snap**
Shift only snaps while placing nodes with the Pen. With no ruler you get 45° steps; with
a ruler you get exactly the ruler angle. If the first node is being placed there is no
anchor, so nothing snaps.

**The boolean submenu is greyed out**
You need at least **two selected siblings**. Divide additionally needs eight or fewer
visible shapes.

**Boolean operations are not in the blend dropdown**
They moved to the layer boolean system on purpose. Surface blends are Derivative / RNM /
Whiteout / UDN / Replace; use `+ → Boolean` for outlines and for the three cut operations.
Legacy projects keep working and show a note in Properties.

**Bevel edges look ribbed or faceted**
Set **Smooth (px)** to 2–4. On diagonal edges this removes the ribbing while keeping the
profile; raising **Softness** fixes it too but changes the bevel shape.

**Smooth has no visible effect**
Make sure the value belongs to the shape you are looking at (a group edits descendants),
that Radius is above 0, and that the document is refreshing on the CPU path (smoothed
stacks fall back to it — watch the evaluation time in the status bar).

**The normal map looks inverted (green) in Unreal or another DirectX tool**
Set the export card's **Normal Y** row to **DirectX (Y−)** before exporting (CLI:
`--normal-directx`). The preview does not flip because it shows the surface, not the encoded
texture; import the map back with the same convention.

**The height map has glowing corners, drift or different slot depths**
The export is probably using the default **Integrated (normals)** source. Set **Height →
Direct (layers)** (CLI: `--export-height=direct`), keep 16-bit and Document resolution, and
export again. Direct mode reads the layer stack and does not integrate the normal field.

**There is no Normal Y row in the export card**
It appears only while the **Normal** chip is enabled.

**Cut pieces keep their old look**
Pieces inherit the **target's** style at the moment of the cut. Change `Smooth`,
`Radius` or `Height` on the target first, then cut again (undo restores the cutter).

---

## 12. Optional: sci-fi texture generator

Not part of the tool rail and not wired into the layer stack: it is a standalone,
self-contained generator that lives in its own window and only writes PNG files.

1. Open **Tools → Sci-Fi texture generator...** A floating window appears with its own
   preview and controls; closing it leaves the editor exactly as it was.
2. Pick a seed, palette, layout (**Octagon grid** / **Trapezoid rows**), resolution
   (512 / 768 / 1024 px) and symmetry (**None** / **Mirror X** / **Mirror XY**, which makes
   the result tileable).
3. Shape the panels with **Chamfer size**, **Subdivision depth**, **Seam width**, **Detail
   density**, **Wear & grime**, **Emissive glow**, **Light angle** and **Normal strength**;
   **Compositional balance** adds glowing counterweights until the detail mass sits closer
   to the canvas centre.
4. Switch the preview between **Final**, **Albedo**, **Normal**, **Height** and **Emissive**.
5. **Save <view> view...** writes one PNG; **Save all views...** writes all five next to each
   other using the file name's folder.

The window reports what it generated (*panels*, detail counts, imbalance before/after) and
how long it took. Every map comes from the same height field, so the normal, AO and wear
agree with each other. Because the output is just PNGs for now, it cannot disturb the
normal-map workflow; hooking it into layers/exports is a separate step.

---

## 13. Touchpad and pen tablets

### Touchpad

- **Pan the 2D canvas:** two-finger scroll. The content follows your fingers, and
  **Shift+scroll** pans sideways. Turn this off in **Settings → Input → Two-finger
  touchpad scroll pans the 2D canvas** if you prefer two-finger scrolling to zoom.
- **Zoom the 2D canvas:** pinch, or hold **Ctrl/Cmd** and scroll. The document point under
  the pointer stays fixed while zooming.
- **3D preview:** pinch or Ctrl/Cmd+wheel dollies the camera. A horizontal two-finger
  swipe orbits yaw, a vertical swipe dollies, and a touch-screen two-finger drag orbits
  the model directly. Dragging still orbits as before.
- The Pan tool, **Space+drag** and middle-drag keep working alongside the gestures.

### Pen tablets

- Pressure scales the brush **size** and **flow** while painting height or a mask. Light
  strokes stay at 20% of the configured size, so the stroke tapers without disappearing.
- Open **Pen** in the mask toolbar, the **Pen pressure** icons in a height-paint
  layer's Properties, or **Settings → Input** to toggle the master switch and the size,
  flow and opacity switches, and watch a live pressure meter. Hover an icon for its hint.
- Pen input uses the same primary-button painting as a mouse, so click, drag, lasso,
  shape and vector tools work without extra setup. Pressure is read directly from Windows
  Ink pointer events (`WM_POINTER`), including hover, so the pressure meter reflects the
  stylus as soon as it comes near the tablet.
- If **Settings → Input** says no pen was seen after touching the canvas, the driver is
  sending mouse-only input. Painting still works, but pressure stays unavailable until the
  driver emits Windows Ink pointer data. For **OpenTabletDriver**, install the
  **Windows Ink** output plugin (Kuuuube/VoiDPlugins) in the Plugin Manager, then select
  it in the profile's **Output Mode**; Absolute and Relative mouse modes send no pressure.
  For Wacom and Xencelabs drivers, enable **Windows Ink** in their settings.
- Run only one tablet driver at a time. Several services (for example Wacom, Xencelabs and
  OpenTabletDriver) competing for the same tablet can leave the pen cursor-only or
  inactive. Wintab-only mode still paints through the mouse fallback but cannot report a
  pressure value.
- The eraser tip and barrel buttons are not mapped yet; use **X** to swap mask reveal/hide
  and the **Smooth** brush for softening.

---

## 14. Blender bridge (live maps)

The add-on in `blender/n_shade_bridge` puts the editor's maps inside a blend file and
keeps them updating while you paint. It is optional and switchable: **Settings > Blender
Bridge > Serve maps to Blender** opens the listener at any time, `--bridge` opens it at
startup, and unticking the box closes it again (the pairing file is removed with it).

### Turn it on

The bridge has a window of its own. Open it three ways:

* click the **Blender · off / ready / live** chip at the right end of the status bar,
* **View → Blender bridge…**, or
* **Settings → Blender Bridge → How to connect…**.

The window holds the switch (**Serve maps to Blender**), the port, the pairing file path,
frames served, the three Blender-side steps with **Copy add-on folder** / **Open folder**,
and **Send all** / **Send selected** for pushing maps on demand.

```powershell
n-shade.exe --bridge                      # start with the listener on
n-shade.exe --bridge --bridge-port 47200  # optional: pick the port
```

The status chip reads **off** (no listener), **ready** (listening, no client) or **live**
(Blender attached) - the fastest way to tell whether the other side is connected. The host
writes a pairing file (`%LOCALAPPDATA%\n-shade\bridge.json` on Windows). The listener binds
loopback only and requires the token in that file, so other programs - including web pages
probing localhost - cannot read the document.

### Install the add-on (Blender 4.3 and newer)

The portable archive ships it at `N-shade\blender\n_shade_bridge` - the bridge window's
**Open folder** button lands exactly there - and a source checkout has it at
`blender/n_shade_bridge`.

1. *Preferences > Get Extensions > Install from Disk* and pick `blender/n_shade_bridge.zip`
   (the portable build ships the same files at `N-shade\blender\n_shade_bridge`; picking
   that folder works too), then enable **N-shade Bridge**. Updating is the same step with
   the newer zip - restart Blender afterwards, because reloading scripts does not re-import
   the add-on's modules.
2. Open **3D Viewport > Sidebar (N) > N-shade**. With the bridge on in N-shade the panel
   pairs and connects by itself; **Auto-pair** and **Connect** do it by hand.
3. Leave **Stream while editing** on to follow the map you are painting, or turn it off and
   use **Pull now** / **Get selected** / **Get all maps**, or the **Send to Blender** button
   in N-shade's export card.
4. Press **Update material** to wire everything into a Principled BSDF.

### Panels

| Panel | What it does |
|---|---|
| **N-shade** | Connection, host version, texture memory, frames sent, round-trip time, the realtime toggle, the map the stream follows, live size, **Pull now**, **Send mesh to N-shade**, **Blender defaults** |
| **N-shade Maps** | Pull size, 16-bit height, the catalogue of every map the host offers with tick boxes, **Get selected** / **Get all maps** / **All** / **None** |
| **N-shade Material** | Material name, normal strength, packed ORM, AO into base colour, bump from height, auto-update, **Update material**, **Release images** |

**Send mesh to N-shade** writes the selected mesh (UVs, normals, positions) to one
scratch file and asks N-shade to import it, so the document is painted on the same UV
layout and the UV overlay matches Blender. Re-sending overwrites that file, so the button
never accumulates meshes on disk.

### Saves stay small

Streamed maps are real image datablocks, and Blender writes any generated image a
material uses into the `.blend`. Seventeen 4K maps would add about a gigabyte to each
save, and again to its `.blend1` backup. With **Keep saves small** on (the default), the
add-on drops every streamed buffer to a single texel just before the write and asks
N-shade for the pixels again right after: node links survive, the file stays small, and
the viewport refills within a frame or two. **Release images** in the Material panel
deletes them outright, and *Max frames per second* in the add-on preferences caps how
often streaming refreshes them.

### What drives an update

Blender asks, N-shade answers, and only one request is in flight. A live request carries
`wait_ms`, so the host **holds it until the document changes** (or answers `UNCHANGED` when
the wait expires). That is what makes the stream feel immediate: a brush stroke reaches
the viewport on the next frame instead of at the next poll, while an idle session moves a
few dozen bytes per second and runs no evaluation at all.

Pushes travel the other way. **Send to Blender** in the export card (or in the PBR panel)
raises a publish generation in N-shade; the next reply - even the `UNCHANGED` reply of an
idle session - carries the list of pushed maps, and the add-on fetches them at the pull
size. Manual mode therefore needs no polling to stay in sync.

| Setting | Effect |
|---|---|
| Stream while editing | On: follow one map continuously. Off: only pulls and pushes |
| Streaming map | Which map the live stream follows; a `Pull now` at full size pins it |
| Live size | Longest side of a streamed frame: 512, 1024 (default), 2048, 4096, Native |
| Pull size | Longest side of **Pull now**, **Get selected**, **Get all** and pushes (default 2048) |
| 16-bit height | Transfer height and displacement as RGBA16 into a full-precision float image |
| Pause while rendering | No image writes while RENDER or BAKE owns the depsgraph |

### What arrives

N-shade announces its catalogue on connect and again whenever it changes, so the Maps
panel only ever lists maps the host can actually serve:

* **Document** - Normal, Height, Curvature, Edge Wear, Edge Detect, ID Map: the same maps
  the export card writes, from the layer stack you are painting.
* **PBR** - Normal, Height, AO, Cavity, Curvature, Edge Wear, Roughness, Metallic,
  Displacement, Base Colour, and a packed **ORM** (R=AO, G=roughness, B=metallic). These
  appear as the single-image PBR pipeline finishes their stages.

**Blender defaults** sets the sizes and material options Blender wants (live 1024, pull
2048, 16-bit height, packed ORM, AO multiplied into base colour, bump when there is no
normal map) and ticks normal, height, curvature and edge wear.

The material uses only stable nodes: Image → Normal Map → **Normal**, Image → **Base
Color**, ORM → Separate Colour → **Roughness** / **Metallic**, and AO multiplied into base
colour the way Blender's own glTF importer does it, because the Principled BSDF has no
occlusion input.

### Memory

A frame is raw RGBA8 (or RGBA16 for the maps that ask for it), the same data the host
already previews with, so the cost is one image per map plus one temporary float buffer
per update: 1024 px is 4 MiB on the wire and a 4 MiB byte image, 2048 px is 16 MiB, and
4096 px is 64 MiB. A 16-bit height image goes into a float buffer instead (four bytes per
channel), so 2048 px of 16-bit height costs 64 MiB on the Blender side. The add-on reuses
one datablock per map and resizes it in place, so node links survive and nothing
accumulates in the file. **Release images** hands everything back.

### Notes

- The stream is always OpenGL (Y+) because Blender shades with Y up. Use the export card's
  **Normal Y: DirectX** for engines that want flipped green; the live stream never flips.
- The images are generated and not fake-users. If a save dropped an unwired image, press
  **Pull now** or wire it to a material.
- Each `.blend` remembers its own selection, sizes and material options on the Scene; port,
  token and startup pairing live in the add-on preferences.
- The addon targets Blender 4.3 through 5.x. Nothing compares version numbers: node and
  socket *names* are looked up with fallbacks, so a localised UI or a renamed socket
  degrades gracefully instead of breaking the material.

### If nothing arrives

| Symptom | Cause |
|---|---|
| "No bridge.json" | The bridge is switched off in N-shade |
| "pairing token does not match" | The app restarted; press **Auto-pair** (auto-connect does it on its own) |
| Connects but no image | No map generated yet in the PBR panel, or **Stream while editing** is off |
| Updates feel heavy at 4K | Lower **Live size**; 4096 costs a 64 MiB frame |
| Panel shows an old error | It clears on the next successful frame |

---

## 15. Text and fonts

Text is a live shape layer: glyph outlines are rasterized on demand, so the normal is
beveled from real letter geometry. Nothing is flattened until you cut, trim or rotate the
layer.

### Place and edit

| Step | Action |
|---|---|
| 1 | Press **T** or click the **Text** tile in the tool rail. |
| 2 | Click the canvas once. The anchor becomes the top-left of the block and the tool returns to Select. |
| 3 | Type in **Properties → Text → Content**. **Enter** starts a new line. |
| 4 | Set **Size (px)**, **Tracking**, **Line height** and **Align**. |
| 5 | Use the Shape section for Radius, Height, Profile, Style and masks. |

The Content field is focused automatically after placement. The layer is named after its
content until you rename it in the **Name** field. Drag the block to move it, Alt-drag to
duplicate and use the union handles to resize; resizing scales the em size uniformly.

### Fonts

| Control | Behaviour |
|---|---|
| **Font** | Inter, Inter SemiBold and JetBrains Mono are embedded in the executable. |
| **Load font file...** | Adds a `.ttf`, `.otf` or `.ttc` file and shows its family name. |
| Stored reference | The project keeps the file path, not the font bytes. |
| Missing file | The text renders with Inter and Properties shows **Missing · using Inter**; re-link the file or choose a bundled face. |
| License | The embedded faces are SIL OFL 1.1; see `assets/fonts/OFL.txt`. |

Layout uses explicit lines (no automatic wrap), applies the font's kerning pairs and then
adds **Tracking** between glyphs. **Align** shifts each line inside the widest line's width.

### Bevel, outline and carve

| Look | Settings |
|---|---|
| Raised lettering | **Style: Fill**, Radius below the stem width, Height to taste. |
| Outline lettering | **Style: Outline stroke**, **Stroke (px)** 6-10 px for 96 px type; keep Radius at or below half the band. |
| Engraved lettering | **Carve (invert)** on a raised panel. |
| Crisp small type | **Anti-alias** 0.3-0.8 px; use **Smooth** for noisy faces. |
| Soft bevel | **Softness** (widens the slope; unlike Smooth it changes the profile). |

0.1.5 fixes the stroke distance to a true signed distance. Before this build, empty pixels
far from the glyphs could be treated as boundary, so an outline text block showed a faint
plate in the height view. The band now stays on the outline.

### Text as vector geometry

- **Vector booleans**: put text and a shape in a group, then choose Add, Subtract, Intersect,
  Xor or Divide. The combined glyph outlines are beveled as one surface.
- **Knife**: text can be the cutter (its glyph outlines cut the shapes below) or the target.
- **Lasso Trim shape** on text replaces it with editable path pieces. **Erase pixels** keeps
  the text live and cuts its painted mask instead.
- **Create outline layer** turns the glyph contours into a new stroked path layer.
- **Clipping and Material ID** work with text on either side of the clip, and the ID map
  follows glyph coverage and painted masks.

Rotating a text layer bakes its outlines into a path, like a primitive; undo restores the
live text. Moving and resizing never flatten it.

### Preview and export

Text is evaluated once on the CPU and uploaded as a precomputed normal/height texture,
so a text layer no longer forces the whole stack onto the CPU evaluator. The status bar
stays on **GPU**. Export re-evaluates the same document on the CPU at the requested
resolution, so text contributes to `normal.png`, `height.png`, `curvature.png`,
`edge_wear.png`, `edge_detect.png` and `id.png` exactly like any other surface.

Tested in 0.1.5: text placement and content focus, bundled and custom fonts, missing-font
fallback, glyph holes, text bevels, normal/height views, the 3D preview and a normal/height/ID
export. See `output/manual_v3/manual.html` for the illustrated version.

---

## 16. Partial cut, partial slash and clip divide

These are three boolean operations, alongside Add, Subtract, Intersect, XOR and Divide. There is
no separate layer kind: choose the operation for a boolean group and the group's bottom shape
becomes the base while the shapes above it cut into it.

### Create one

1. Select the base shape (the plate) and one or more cutter shapes above it.
2. Open **Boolean** in the `+ Add` menu, the layer row's context menu, or the group's
   Properties, and choose **Pocket**, **Slash** or **Clip**.

The shapes move into a boolean group, the bottom-most shape is the base, and the cutters stay
editable. Drag, rotate, scale or node-edit a cutter exactly like any other shape. To mix
operations, nest them: select a finished cut group plus the next cutter and apply another cut
operation, as in the demo (`--demo-cuts`).

### What each operation does

| Operation | Result |
|---|---|
| Pocket (partial cut) | Subtracts each cutter's height profile from the base and stops at the base floor. |
| Slash (partial slash) | A pocket cut tapered along each cutter's long axis, so the groove fades at both ends. |
| Clip divide | Cuts through the base along each cutter's band. The band's bevel becomes the two seam walls, and **Step** raises the far side. |

### Controls

The operation button lives in the group's **Boolean** section. Each cutter keeps its own
**Shape** section, where Depth, Radius and Profile are the cut's depth, wall width and wall
curve. Selecting a cutter also exposes the mode-specific parameters:

* **Slash**: Taper in and Taper out (fractions of the long axis, 0 to 0.5).
* **Clip divide**: Step (px), a height offset on one side of the seam.

Notes:

* The bottom-most visible shape is always the base. A cut group's coverage is the base's
  coverage, so material IDs stay on the base while cutters only remove height.
* Pocket and slash never remove more than the base has; the floor is the base of the stack.
* A slash or divide cutter needs some length for its long axis; a square shape still works but
  its axis is arbitrary.
* Cut groups are evaluated on the CPU because they need the base's height field. The status bar
  shows **Vector compound** while they are active.
* **Bake cut to shapes** removes the live cut and keeps the base and cutters as editable
  layers with the cut blends. Use **Bake to pixels** on the group when you want one baked
  normal layer instead of editable shapes.

---

## 17. Project normal shapes onto the 3D model

The 3D viewport doubles as a projection workspace: a layer's normal-map content can be wrapped
onto the imported mesh with a box (cube) projection, then committed back into the 2D canvas.

### Start a projection

1. Import a UV-mapped mesh (**Mesh** tab, or drop a `.glb`, `.gltf`, `.obj`, `.fbx`, `.dae` or
   `.usd` file).
2. Select a shape, group, normal layer or painted-height layer in the Layers panel.
3. In the 3D viewport header, click the cube-projection icon (the tile immediately after
   **Split**).

A cyan cage appears around the model and **3D Projection** opens at the top of the Properties
dock. The eye icon in the card switches between the projected and live preview.

### Adjust the cage

* **Centre dot** - drag to move the box in the view plane.
* **Coloured arrows** - drag along X (red), Y (green) or Z (blue).
* **Corner square** - drag to scale the box uniformly.
* **Twist ring** - drag around the centre to rotate in the view plane.
* The Properties card also has exact **Move**, **Size** and **Rotate** fields, plus **Sharp**
  (edge crispness between projection axes) and **Feather** (soft fade outside the box).
* **Fit** snaps the cage to the mesh bounds. **Reset** returns to a unit box. **Esc** or
  **Cancel** closes the tool without changing the document.

### Apply back to 2D

**Apply to 2D** re-projects at the document resolution, hides the source layer, inserts a new
`<name> projected` normal layer directly above it (with the projection coverage as its mask),
and selects it. The 2D canvas therefore shows the same normal detail that was on the model.
One **Ctrl+Z** undoes both the insertion and the hidden source.

Notes:

* The preview is intentionally computed at a small resolution for interactive feedback;
  Apply always re-projects at the document size.
* Bottom-left UV formats (OBJ/FBX/COLLADA/USD) and top-left glTF UVs are both handled, so the
  committed layer lands upright in the 2D canvas for every supported import format.
* The source is hidden, not deleted. Turn its eye back on, or delete the projected layer, to
  return to the original stack.
