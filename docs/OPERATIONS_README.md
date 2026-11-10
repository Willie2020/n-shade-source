# Operations: Implementation Guide (Rust + Bevy)

This document guides a developer (or an AI coding assistant) in adding an **Operations** system to an existing, working vector-based normal-map authoring tool **without breaking anything that already works**.

The full feature specification lives in `operations.md`. Keep that file in the repo (for example `docs/operations.md`) as the source of truth for *what* each operation does. This README covers *how* to add them safely.

> **Read this first:** the existing application works. Every change must be additive. If a step would require rewriting existing behavior, stop and find a smaller seam instead.

---

## 1. Ground rules (non-negotiable)

1. **Additive only.** Add new modules, plugins, resources, and components. Do not rename, move, or change the signature of existing public items unless unavoidable. If unavoidable, make it its own commit with a note.
2. **Do not change existing behavior.** With the Operations feature disabled or unused, the app must behave exactly as before: same rendering, same output, same file format.
3. **One operation at a time.** Each operation lands in its own commit/PR with tests. Never add several half-working operations at once.
4. **Operations are pure.** Evaluating an operation reads its input and parameters and returns a result. It does not mutate the document, spawn entities, or touch the GPU. Applying the result is a separate, undoable step.
5. **Deterministic.** Same input + same parameters + same seed = identical output, every run, every machine.
6. **Everything undoable.** Every applied operation is one undo step (one undo reverses the entire operation, not part of it).
7. **Old project files must still open.** Any new saved data is optional and defaulted.
8. **Build and test green at every commit.** Run `cargo fmt`, `cargo clippy`, `cargo test`, and a manual smoke test of the existing workflow before each commit.

---

## 2. Step 0: Audit the existing code (do this before writing anything)

Before adding code, read the project and write a short `docs/architecture-notes.md` answering these questions. **Do not guess; open the files.**

| Question | Why it matters |
|---|---|
| Which Bevy version is in `Cargo.toml`? | Event/message APIs, plugin APIs, and system sets differ between versions. |
| How are vector shapes stored? (ECS components, a document struct in a `Resource`, something else?) | Operations must read and write shapes through the same model. |
| What is the shape/path type? (own type, `lyon`, `kurbo`, `geo`, etc.) | Reuse it. Do not introduce a second path type. |
| How is the height map produced? (CPU buffer, GPU pass, compute shader?) | Decides how height contributions are combined and previewed. |
| How is the normal map generated from height? | Normal Strength and Flip must hook in at the right stage. |
| Is there already undo/redo? How does it work? | Reuse it. If none exists, see section 7. |
| How do tools, selection, and input work currently? | Operations must not fight existing tools for input. |
| How is UI built? (`bevy_egui`, `bevy_ui`, other) | The Operations panel must use the same UI stack. |
| How are projects saved/loaded? (serde? format? versioned?) | New data must be backwards compatible. |
| What are the document units and output resolution settings? | Operations must respect them. |
| What tests exist? | Know the safety net before changing anything. |

Also record the **list of existing behaviors that must keep working** (draw shape, edit path, select, move, export normal map, save/load, and so on). This becomes the regression checklist in section 11.

**Deliverable of Step 0:** the notes file plus a baseline: run the app, export a normal map from a few sample projects, and save those outputs as golden reference files (see section 11).

---

## 3. Architecture overview

Add one self-contained module tree and one Bevy plugin. The rest of the app only needs a single line to register it.

```
src/
  operations/
    mod.rs              // OperationsPlugin + public surface
    registry.rs         // OperationRegistry resource
    traits.rs           // Operation trait, input/output types
    params.rs           // parameter definitions and values
    context.rs          // EvalContext (units, resolution, seed, quality)
    command.rs          // Apply/Undo integration
    preview.rs          // preview state + systems
    height.rs           // raised/recessed, profiles, combine modes
    limits.rs           // safety limits and warnings
    ui.rs               // searchable Operations panel
    ops/
      array.rs
      radial_array.rs
      mirror.rs
      offset_path.rs
      stroke_to_shape.rs
      panel_groove.rs
      vent_slot.rs
      rivet_fastener.rs
      circular_radial.rs
      emboss_engrave.rs
      bevel_profile.rs
      ...
tests/
  operations_*.rs
```

### Data flow

```
 selection + params
        |
        v
 Operation::evaluate()   <-- pure, deterministic, no side effects
        |
        v
 OperationOutput { new shapes, modified shapes, height contributions, warnings }
        |
   +----+-----------------------------+
   |                                  |
 Preview (temporary, not in document) Apply (one undoable Command)
   |                                  |
 overlay entities / temp buffer       document updated, then height/normal regenerated
```

The key idea: **preview and apply use the same `evaluate()`**. Preview just shows the output without committing it.

---

## 4. Core types (illustrative sketch)

These are a starting point. **Adapt the names and types to the existing codebase** (especially the shape and path types found in Step 0). Do not copy blindly.

```rust
/// Stable identifier. Never change an id after release (saved files may reference it).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct OperationId(pub &'static str);

/// Tells the user (and the UI) what kind of result this operation produces.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OutputKind {
    NewVectorShapes,      // creates new vector objects
    ModifiesSelection,    // edits the selected geometry
    HeightContribution,   // changes the height representation
    NormalOnly,           // changes the normal result only
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Quality { Fast, Final }

pub struct OperationMeta {
    pub id: OperationId,
    pub name: &'static str,
    pub category: Category,
    pub keywords: &'static [&'static str],   // for search
    pub output_kinds: &'static [OutputKind],
    pub shortcut: Option<&'static str>,
}

pub struct EvalContext {
    pub units_per_pixel: f32,       // from document units + output resolution
    pub output_resolution: UVec2,
    pub quality: Quality,
    pub limits: Limits,
}

pub trait Operation: Send + Sync + 'static {
    fn meta(&self) -> &OperationMeta;
    fn param_schema(&self) -> ParamSchema;          // drives the UI and validation
    fn default_params(&self) -> ParamValues;
    fn validate(&self, input: &OperationInput, p: &ParamValues) -> Vec<ParamIssue>;
    fn evaluate(
        &self,
        ctx: &EvalContext,
        input: &OperationInput,
        p: &ParamValues,
    ) -> Result<OperationOutput, OperationError>;
}

pub struct OperationOutput {
    pub new_shapes: Vec<Shape>,                    // use the app's existing shape type
    pub modified_shapes: Vec<(ShapeId, Shape)>,
    pub height: Vec<HeightContribution>,
    pub warnings: Vec<OperationWarning>,           // e.g. "details below 1.5px"
}

pub struct HeightContribution {
    pub shapes: Vec<Shape>,
    pub direction: Direction,        // Raised | Recessed
    pub height: f32,
    pub profile: EdgeProfile,        // Sharp | Rounded | Chamfer | Stepped | Soft | Custom
    pub edge_width: f32,
    pub combine: CombineMode,        // Add | Subtract | Max | Min | Replace | Mask
}
```

### Parameter system

Define each operation's controls as a **schema** (name, type, min, max, default, step, unit, visibility rule). The UI is generated from the schema, so:

- only relevant controls are shown for the selected operation,
- invalid values (negative counts, zero spacing) are clamped or rejected in one place,
- adding a new operation does not require new UI code.

Shared controls from the spec (Amount, Width, Height/Depth, Profile, Spacing/Count, Angle, Offset, Random seed, Clip to selection) should be **reusable parameter definitions**, not re-declared per operation.

### Registry

```rust
#[derive(Resource, Default)]
pub struct OperationRegistry { /* ordered map: OperationId -> Box<dyn Operation> */ }
```

- Use an ordered structure (`Vec` + index, `IndexMap`, or `BTreeMap`) so listing order is stable.
- Each operation registers itself in `OperationsPlugin::build`.
- The UI search reads from the registry (name, category, keywords).

---

## 5. Bevy integration

Create a single plugin and add it to the app in one line:

```rust
pub struct OperationsPlugin;

impl Plugin for OperationsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<OperationRegistry>()
           .init_resource::<ActiveOperation>()
           .init_resource::<OperationPreview>()
           // register events/messages (use the correct API for the project's Bevy version)
           // add systems inside a dedicated SystemSet
           ;
        // register_all_operations(app);
    }
}
```

Guidelines:

- **Check the Bevy version first.** Event, message, observer, and system-set APIs changed across recent releases. Use whatever the existing code already uses.
- Put all new systems in their own `SystemSet` (for example `OperationsSet`) and order it explicitly relative to the existing input, selection, and render sets. Do not reorder existing systems.
- **Never block the main thread** on heavy evaluation (see Preview, section 6).
- Use marker components (for example `OperationPreviewEntity`) for any temporary entities so they can be found and despawned cleanly.
- Keep operation logic **out of systems**. Systems only gather input, call `evaluate()`, and forward the result. This keeps the logic unit-testable without a Bevy `App`.
- **Cargo feature flag:** gate the plugin behind `operations` while developing.
  ```toml
  [features]
  default = []          # switch to ["operations"] once the regression checklist passes
  operations = []
  ```
  Then `#[cfg(feature = "operations")] app.add_plugins(OperationsPlugin);`

### Input handling

- The Operations panel and its tool state must **not capture input** unless an operation is active.
- While an operation is active, route input through the same mechanism existing tools use, and make sure pressing Esc cancels and Enter applies.
- Keyboard shortcuts must not collide with existing ones. Build a list of current shortcuts in Step 0 and check against it.

---

## 6. Preview system

Requirements from the spec: immediate preview, fast mode while dragging, high-quality result on commit.

- **Preview never touches the document.** It holds `Option<PreviewState>` in a resource containing the latest `OperationOutput` and the params that produced it.
- Render preview as temporary overlay entities and/or a temporary height/normal buffer. Despawn everything on Cancel and on Apply.
- **Fast vs Final:** while a control is being dragged, evaluate with `Quality::Fast` (for example fewer curve subdivisions, lower-resolution height preview). On release or Apply, evaluate with `Quality::Final`.
- **Async for heavy work:** run evaluation on `AsyncComputeTaskPool` when it may exceed a frame budget. Attach a monotonically increasing **generation number** to each request and discard results whose generation is stale.
- **Debounce** parameter changes so dragging a slider does not queue hundreds of evaluations.
- Surface **warnings in the preview**: details too small to survive rasterization, shape count above the soft limit, invalid input (such as an open path where a closed one is required).

---

## 7. Apply, Cancel, Undo/Redo

- If the app already has undo/redo, **implement Apply through it**. Do not build a second history system.
- If it has none, add a minimal command stack: each command stores enough to undo (the shapes it added, and the original versions of any shapes it modified) and to redo.
- One Apply = one command = one undo step.
- **Preserve the original selection** unless the operation explicitly edits it (per spec). Newly generated shapes should be selected afterward only if this matches how existing tools behave.
- **Apply / Convert:** where an operation supports a live (editable) result, provide a "Convert to shapes" action that bakes it into ordinary vector shapes. Start with bake-on-apply (simplest and safest). Add live-editable operations later, one at a time, once the baseline is solid.

---

## 8. Height and normal handling

This is the most sensitive area because it touches the core output. Rules:

1. Add new height contributions through the **existing height pipeline**. Do not create a parallel one.
2. Support the combine modes from the spec: **Add, Subtract, Max, Min, Replace**, plus Mask if the pipeline supports it. Implement them as small pure functions with unit tests.
3. **Avoid spikes on overlap.** Default overlapping details *within one operation* to `Max` (or the safest mode for the pipeline). Make `Add` a deliberate choice.
4. Keep **height polarity** explicit (Raised = positive, Recessed = negative in one agreed convention). Document the convention in code.
5. **Normal Strength** scales the normal response only; it must not change vector geometry or stored height.
6. **Invert / Flip:** keep height inversion and normal-channel flip (for example green channel, OpenGL vs DirectX) as separate, clearly labeled options.
7. Edge profiles (Sharp, Rounded, Chamfer, Stepped, Soft) should be implemented once as a shared function mapping *distance from edge* to *height factor*, then reused by every operation.

---

## 9. Dependencies

**Prefer what is already in `Cargo.toml`.** Only add a crate if something truly needs it, and justify it in the commit message. Candidates to evaluate (check current versions and licenses first):

| Need | Candidates |
|---|---|
| Stroking, path offset, curve math | `kurbo` |
| Path tessellation / strokes | `lyon` |
| Polygon booleans (union, subtract, intersect, XOR) | `i_overlay`, `geo` with a clipping backend, `clipper2` bindings |
| Polyline offset | `cavalier_contours`, or kurbo/clipper offset |
| Deterministic randomness | `rand` with a seeded RNG such as `rand_pcg` or `rand_chacha` |

Rules:

- **Never** use `thread_rng()` or any unseeded randomness in an operation. Always derive the RNG from the `Random seed` parameter.
- **Never** let hash-map iteration order affect output. Use ordered collections.
- Floating-point results must be stable. Avoid parallel reductions whose order can change the result.

---

## 10. Operations to implement, in order

Follow the recommended initial release from the spec. Each row is a separate PR.

| # | Operation | Output kind | Notes |
|---|---|---|---|
| 0 | **Foundations** (registry, params, preview, command, UI panel, limits) | n/a | Ship with one trivial operation (Duplicate and Nudge) to prove the pipeline end to end. |
| 1 | Array | New shapes | Linear/grid, per-row/column offset, rotation/scale variation. |
| 2 | Radial Array | New shapes | Center, angular range, orient-with-placement toggle. |
| 3 | Mirror | New shapes | Horizontal, vertical, custom axis. |
| 4 | Offset Path | New shapes | Join styles, multiple steps. Needs robust handling of self-intersection. |
| 5 | Stroke to Shape | New shapes | Caps, joins, miter limit. |
| 6 | Bevel Profile + Raised/Recessed + Height/Depth | Height | Build the shared height/profile layer here. Later operations depend on it. |
| 7 | Emboss and Engrave | Height | First end-to-end vector-to-normal operation. |
| 8 | Panel and Groove | Shapes + height | Uses Offset Path and profiles. |
| 9 | Vent and Slot | Shapes + height | Uses Array/Radial Array. |
| 10 | Rivet and Fastener | Shapes + height | Presets, along-path placement can wait for path repetition. |
| 11 | Circular and Radial Details | Shapes + height | Rings, bolt circles, radial marks. |

**Phase 2 (only after the above are solid):** Boolean Operations, Corner Treatment, Perforation, Ribbing/Fluting, Repeat Along Path, Stitch and Seam, Scatter, Greeble Placement, Scratch and Damage, Scale/Tile Pattern, Halftone, Simplify/Smooth, Join/Close, Subdivide.

> **Shipped:** Repeat Along Path is available as the **Sweep** operation. A vector layer
> switched to **Path guide** supplies the spine, copies are spaced by arc length, and
> **Rotate to tangent** turns each copy along the path.

> Booleans and Offset Path are foundations for many later operations. If Panel and Groove or Perforation need them, implement and test those first.

### Definition of done for each operation

- [ ] Parameter schema with defaults, ranges, and validation
- [ ] Pure `evaluate()` with no side effects
- [ ] Deterministic (seeded; verified by a test that runs it twice and compares)
- [ ] Preview works (fast and final quality)
- [ ] Apply is one undo step; undo and redo restore the exact state
- [ ] Clear indication of what it creates (new shapes, modified shapes, height, normal)
- [ ] Unit tests for edge cases (empty selection, single point, open vs closed path, zero and extreme values)
- [ ] Respects document units and output resolution
- [ ] Warns about too-small details and excessive shape counts
- [ ] Appears in the searchable panel under the correct category
- [ ] Existing regression checklist still passes

---

## 11. Testing and regression safety

### Baseline protection

- Before any change, export a **golden set**: normal maps (and saved projects) from several representative existing projects. Store them in `tests/golden/`.
- Add a test that loads each project and re-exports it, comparing against the golden output (exact match, or within a documented tolerance if the pipeline has float noise).
- Add a **load-old-file test**: a project saved by the pre-Operations version must open and export identically.

### Unit tests per operation

- Geometry results for known inputs (for example Array of 3 copies with spacing 10 yields shapes at 0, 10, 20).
- Determinism: same seed gives identical output; different seed gives different output (for randomized operations).
- Invalid parameters: clamped or rejected, never a panic.
- Degenerate input: empty selection, zero-length path, collapsed shape, self-intersecting path.

### Height and normal tests

- Combine modes (Add, Subtract, Max, Min, Replace) on overlapping shapes: no spikes, correct polarity.
- Raised vs recessed produce opposite normal directions.
- Normal Strength scales the response without changing the height buffer.
- Flip affects only the intended channel.
- Test at **several output resolutions** (for example 512, 1024, 2048, 4096) for edge quality, antialiasing, and overlap behavior.

### Manual smoke test (run before every commit)

1. Open the app. Draw a shape, edit its points, move it, delete it.
2. Undo and redo through all of the above.
3. Save, close, reopen. Confirm the project is intact.
4. Export a normal map. Compare to the golden export.
5. Run the new operation: preview, cancel, apply, undo, redo.

---

## 12. Performance and safety limits

- **Soft limit:** warn when an operation would generate more than a configurable number of shapes (suggest ~2,000). **Hard limit:** refuse above a larger cap (suggest ~20,000) with a clear message. Keep both in `limits.rs` and make them configurable.
- **Minimum feature size:** flag details smaller than roughly 1 to 2 output pixels at the current target resolution.
- Never panic on user input. Return `OperationError` and show it in the UI.
- Validate before evaluating: reject zero spacing, negative or zero counts, NaN/infinite values.
- Keep memory bounded: avoid allocating per-shape heap data when a compact representation will do.

---

## 13. File format compatibility

- Any new serialized data (operation settings, provenance) must use `#[serde(default)]` and optional fields.
- **Never rename or remove existing fields.**
- If the format is versioned, bump the version only if required, and keep a loader for the old version.
- Applied operations that bake to ordinary shapes need **no format change at all**. Prefer this for the first release.
- Operation ids are permanent. Changing one later breaks saved files.

---

## 14. Suggested workflow and commit plan

1. `docs: architecture notes and golden baseline` (Step 0, no code changes)
2. `feat(operations): module skeleton, registry, plugin behind feature flag`
3. `feat(operations): parameter schema and validation`
4. `feat(operations): preview resource and systems`
5. `feat(operations): undoable apply command`
6. `feat(operations): searchable Operations panel`
7. `feat(operations): Duplicate and Nudge (pipeline proof)`
8. One commit per operation after that (section 10 order)
9. `chore: enable operations feature by default` (only after the full regression checklist passes)

---

## 15. Instructions for an AI coding assistant

If you are an AI assistant working on this repository, follow these rules:

1. **Start with Step 0.** Read the codebase and produce `docs/architecture-notes.md` before writing feature code. Do not assume file names, types, or the Bevy version.
2. **Change as little existing code as possible.** Prefer adding new files. Explain every edit to an existing file.
3. **Work one numbered step at a time** from section 10 and stop after each so the result can be tested.
4. **Do not invent APIs.** If unsure about a Bevy or crate API for the project's version, check the version in `Cargo.toml` and the crate docs, or ask.
5. **Use existing types** for shapes, paths, selection, undo, and UI. Do not duplicate them.
6. **Keep operation logic pure and testable** outside of Bevy systems.
7. **Write tests with the code.** Do not mark an operation done without its tests (section 10, "Definition of done").
8. **Never delete or weaken existing tests** to make a change pass.
9. **If something in this guide conflicts with the existing architecture,** stop, explain the conflict, and propose the smallest adaptation instead of forcing the guide.
10. **Out of scope:** do not turn this into a node-based procedural material editor. Operations are direct, artist-driven vector and surface-detail tools.

### Starter prompt

> Read `OPERATIONS_README.md` and `docs/operations.md`. Complete Step 0 only: audit the existing code and write `docs/architecture-notes.md` answering every question in the Step 0 table, plus a list of existing behaviors that must not break. Do not modify any source files yet. When finished, summarize what you found and propose the exact files you would add for the foundations step.

---

## 16. Scope reminder

Operations are manual vector and surface-detail editing tools for a normal-map authoring application. The priorities are direct manipulation, practical hard-surface details, rapid iteration, and a clear path from vector shapes to a useful normal map. They are **not** a node-based procedural material system.
