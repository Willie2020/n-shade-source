# N-shade performance baseline and P0 results

**Date:** 2026-10-29  
**Environment:** Windows, Rust 1.99.1, Intel Core i7-12700H, 16 GB RAM  
**Build:** `cargo test --release -p ns_app perf_ -- --ignored --nocapture` (release profile, thin LTO). The same tests were also run under `cargo test` with the same conclusions.

This file records the measurements taken while working through
`OPTIMIZATION_README.md`. It is deliberately limited to what was actually run.

## What was measured

- Microbenchmarks implemented as `#[ignore]`d unit tests, so they add no
  dependency and stay next to the code they exercise.
- Each test builds its fixture in-process and prints one line with
  `cargo test -p ns_app <name> -- --ignored --nocapture`.

| Benchmark | Command |
|---|---|
| Document snapshot clone vs. shared handle | `cargo test -p ns_app perf_document_snapshot_clone -- --ignored --nocapture` |
| Drag geometry clone removed per pointer move | `cargo test -p ns_app perf_drag_source_clone -- --ignored --nocapture` |
| Layer-row rebuild vs. cache hit | `cargo test -p ns_app perf_layer_rows_cache -- --ignored --nocapture` |

## Results

### Task 1 + 2 — shared document snapshots

Fixture: 1024×1024 document, 8 shape layers, 4 painted masks (16 MiB).

| Operation | Before | After |
|---|---:|---:|
| Undo checkpoint / CPU evaluation snapshot | 3.99 ms deep clone | 0.012 µs (12 ns) `Arc` clone |
| Selection-only click | 3.99 ms deep clone that is discarded | 0.012 µs handle clone |
| First real edit in a gesture | document mutated in place | one copy-on-write detach (same 3.99 ms) |

The first actual edit still pays one document copy, which is required so the
snapshot keeps the pre-edit state. The win is that selection clicks, hover
clicks, context-menu clicks and background evaluation snapshots no longer pay
it. A running CPU evaluation also no longer forces a second clone when an edit
arrives.

### Task 3 — drag sources

`collect_selection_sources` was already called once at drag start (the guide's
premise was only partly true). The per-move cost was a different one:
`canvas_edit_drag_with_constraint` cloned the whole `CanvasDrag` on every
pointer move, including every cached `ShapeSource`.

Fixture: 8 selected layers, each with a 5,000-node path.

| Operation | Before | After |
|---|---:|---:|
| Per pointer move | 0.436 ms cloning cached geometry | 0 ms (drag moved out and back, no clone) |

The drag is now taken out of `EditorState` for the update and restored
afterwards. None of the update helpers touch `canvas_drag`, so the geometry
snapshots stay intact.

### Task 4 — layer rows and labels

Fixture: 256 flat layers.

| Operation | Before | After |
|---|---:|---:|
| Flatten row list + names/type labels | 67.3 µs rebuild per frame | 0.03 µs cache hit |
| Name/type-label allocations per frame | one `String` per row | none (cached `Arc<str>`) |
| Frame where nothing changed | rebuild + allocations | one hash + `Arc` clone |

The row cache key is `(edit_serial, hash(collapsed))`. Selection and hover do
not invalidate it; document edits and collapse/expand do. `LayerRow` now carries
the cached display name and type line, so the row renderer no longer reads and
clones `layer.name` or formats `kind_label` every frame.

## Tasks not started, and why

- **Task 5 (COW boolean grids):** premise does not survive inspection. The
  source grids are `Arc<Grid>` values owned by `EvalCache`; `Arc::make_mut`
  would still clone because the cache keeps a live reference. Avoiding the copy
  needs a different object graph (e.g. evaluating boolean operands into a
  scratch buffer or handing unique ownership out of the cache), which is a
  larger change than the guide's `try_unwrap` suggestion.
- **Task 6 (boolean spatial index):** correctly marked high risk. The guide
  requires a regression and property-test suite plus an old-vs-new oracle
  before changing it; that work was not started.
- **Tasks 7–12:** not attempted. In particular, no GPU or sci-fi changes were
  made, and no build-profile change was measured.

## 0.1.17 — GPU profiles and compound vector paths

**Date:** 2026-10-31

**Fixture:** `showcase/scifi_panel_detailed.nsproj`, 1024×1024, 247 layers, a
327-cell honeycomb, slot banks and bolt rings.

### Root cause

`gpu::build_scene` returned `None` when a shape used any bevel profile other
than Round/Chamfer, or when a path had more than one subpath. The showcase used
Linear profiles and compound paths, so the whole stack fell back to the CPU and
the status bar reported `CPU · 16534 ms`.

### Changes

- All built-in analytic profiles run in the shader now: Linear, Convex,
  Concave and Smooth, alongside Round and Chamfer. The shader uses the same
  `dh/dt` formulas as `ns_core::sdf::profile_slope`.
- Multi-subpath paths are packed as a contour list and evaluated as the union
  of disjoint islands. Holes, open contours and mixed winding still use the CPU
  so the result cannot diverge from the CPU rasterizer.
- GPU packing uses tolerance flattening (`flatten_path_tolerance(0.25)`) instead
  of the rasterizer's fixed 24 samples per segment. A closed square packs as 4
  vertices instead of 96, and a hex cell as 6 instead of 144. The two-square
  compound test packs 8 vertices where the old path packed 192.
- The compound SDF uses per-contour AABBs for branch-and-bound, so honeycomb and
  slot layers skip distant islands before running the edge loop.
- Layer row → **Bake to pixels** rasterizes a shape's draw coverage at document
  resolution. Bevel parameters are kept; a test asserts the evaluated normal
  grid is unchanged (worst delta < 1e-5). Trade-off: raster layers use the CPU
  evaluator and the coverage grid is stored in the project, so the tooltip
  warns about both.

### Observed

- The same 247-layer project now shows the green `GPU` indicator and is fully
  rendered in a capture taken 2 s after opening (previously `CPU · 16534 ms`).
- `cargo test -p ns_app`: 338 passed, 3 ignored. `cargo test -p ns_core`:
  224 passed.

### Next candidates (not implemented)

- Tile binning: build a per-8×8-tile layer list so the shader stops testing
  every layer AABB per pixel.
- A GPU SDF texture atlas (R16F distance field, mipmapped) so baked shapes can
  stay on the GPU instead of switching the stack to the CPU.
- Half-resolution interaction preview with a full-resolution idle pass, plus
  dirty-rectangle compositing for local edits.
- Compact serialization for baked grids (binary/base64/zstd); the current RON
  float array is much larger than the in-memory grid.
- A BVH over contours for compounds far larger than the current honeycomb.

## Not measured

The assessment checklist also asks for puffin scopes, a flamegraph and an
allocation profile. Those were **not** run in this environment:

- no `cargo flamegraph` / `heaptrack` / `dhat` capture was taken;
- the crate has no lib target, so `criterion` benches cannot import `ns_app`
  internals without first adding a library target;
- the numbers above are microbenchmarks, not end-to-end frame timings.

They should still be captured before optimizing Tasks 5–8, so the profile can
confirm which of those matters in practice.

## Validation

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`

The new regression tests cover:

- a gesture snapshot shares the document until the first edit, then detaches;
- evaluation of a shared snapshot is bit-identical to the live document;
- shared snapshots are counted once in the undo memory budget;
- layer-row flattening preserves reverse order, nesting, collapse and labels;
- the layer-row cache survives selection changes and invalidates on edits.

## Unreleased — Phase 0 complete (T0.1–T0.5, T1.5)

**Date:** 2026-11-01  
**Commit:** `1befcbf` plus the working-tree changes below  
**Environment:** Windows 11 10.0.22631, Rust 1.99.1, Intel Core i7-12700H,
16 GB RAM, NVIDIA GeForce RTX 3070 Ti Laptop (driver 32.0.16.1742), Vulkan.  
**Build:** `[profile.profiling]` (inherits release; line-tables-only debug info).

No GPU pass timings are claimed here: the HUD reads them from
`RenderDiagnosticsPlugin`, and they are backend- and driver-dependent. The
numbers below are CPU-oracle timings from the CLI, which are deterministic
enough to compare across changes.

### Added

- **Reason-coded GPU gate.** `gpu::GateReject` names every whole-stack fallback
  reason. `gpu::build_scene_reported` returns all detected reasons while
  `gpu::build_scene_cached` keeps its existing `Option` contract. The status bar
  shows the reason count and lists the reasons on hover.
- **Performance HUD.** The status-bar **Perf** chip opens a floating panel with:
  path (GPU or CPU fallback), fallback reasons, scene-build last/mean/p95, CPU
  evaluation last/mean/p95, GPU eval/height/upload ms, frame time,
  evaluation-cache bytes vs the 256 MiB budget, undo + redo bytes vs the 512 MiB
  budget, document size and visible layer count. `--perf-hud` opens it at
  startup for capture/benchmark runs.
- **GPU timings and spans.** `RenderDiagnosticsPlugin` is installed explicitly
  (Bevy only auto-adds it with `tracing-tracy`). `ns_gpu_eval` and
  `ns_gpu_height` compute passes carry GPU timestamp spans, and `GpuUploadStats`
  shares the measured upload cost with the HUD. `trace_span!` scopes cover
  `undo.checkpoint`, `evaluate`, `gpu.scene_build`, `eval.cpu`, `gpu.upload`,
  `gpu.dispatch`, `export`, `project.load` and `project.save`.
- **Dedicated evaluation pool.** `ns_core::eval_pool()` builds a private rayon
  pool (`available_parallelism - 2` workers, named `ns-eval-*`). The editor's
  CPU evaluation runs inside `install(..)`, so its nested parallel kernels stop
  competing with Bevy's global rayon work.
- **Cooperative cancellation.** `ns_core::CancelToken` plus the cancellable
  evaluator variants on `Document` and the layer stack. The editor keeps a
  `CpuEvalJob { task, token, eval_serial }`; a newer edit or a successful GPU
  pass cancels the superseded worker at layer boundaries instead of letting it
  finish.
- **Parity harness.** `crates/ns_app/src/parity.rs` dispatches the real
  `normal_studio.wgsl` through headless `wgpu`, compares normals and heights
  with the CPU oracle, and reports worst pixels. The first run exposed a real
  GPU AABB rounding bug: analytic layers now use `floor(min - expand)` /
  `ceil(max + expand)`, matching `ShapeLayer::region_rect`, instead of a blanket
  one-pixel margin.
- **Corpus and benchmarks.** `ns_cli bench` writes the fixed `bench/scenes/`
  corpus with stable layer ids and runs cold/warm/edit timings.
  `crates/ns_core/benches/kernels.rs` is a dependency-free kernel benchmark so
  `cargo bench -p ns_core` works without a network fetch.
- **Profiling profile.** `[profile.profiling]` inherits release and adds
  line-tables-only debug info for Tracy/perf/flamegraph:
  `cargo build --profile profiling -p ns_app`.

### T0.1 corpus

| Scene | Purpose | Size | Layers | Project |
|---|---|---:|---:|---:|
| `s01_8shapes_4masks_1k` | undo/selection/drag microbench | 1024² | 9 | 1.7 MiB |
| `s02_101layers_1k` | streaming layer walk | 1024² | 101 | 0.1 MiB |
| `s03_247_showcase` | gate + full showcase stack | 1024² | 247 | 3.0 MiB |
| `s04_honeycomb_compound` | 225-island vector compound | 1024² | 227 | 0.3 MiB |
| `s05_paths_8x5000` | large path flattening | 1024² | 9 | 11.8 MiB |
| `s06_photo_paint_2k` | photo + height paint + imported normal | 2048² | 8 | generated on demand |
| `s07_nested_groups` | non-identity groups, painted group masks | 1024² | 121 | 1.6 MiB |
| `s08_smart_masks` | slope/curvature/noise generators | 1024² | 10 | 1.5 MiB |
| `s09_tileable_repeat` | `Wrap::Repeat` and edge-crossing shapes | 1024² | 8 | <0.1 MiB |
| `s10_4k_20layers` | 4K canvas and export | 4096² | 20 | <0.1 MiB |

`s03` is copied from the tracked `showcase/scifi_panel_detailed.nsproj`.
`s06` is generated in memory by default: its RON grids would be hundreds of
MiB, so saving it is opt-in with `--save-raster`. All other scenes are written
with deterministic layer ids.

```sh
cargo run --profile profiling -p ns_cli -- bench --repeats=3
cargo run --profile profiling -p ns_cli -- bench --scenes=s03 --repeats=3
```

### Gate report

`cargo test -p ns_app perf_corpus_gate_report -- --ignored --nocapture` runs the
reason-coded gate over every saved project without opening a window.

| Scene | Path | Detail |
|---|---|---|
| s01 | GPU | 9 layers, 4 masks |
| s02 | GPU | 101 layers |
| s03 | GPU | 226 GPU layers, 7287 vertices |
| s04 | CPU | `VectorCompound` |
| s05 | GPU | 9 layers, 40000 vertices |
| s06 | not saved | generated on demand |
| s07 | CPU | `NonIdentityGroup`, `GroupPaintedMask` |
| s08 | CPU | `SmartMask` |
| s09 | GPU | 8 layers |
| s10 | GPU | 20 layers |

These are the fixed expectations T1.1 must change on purpose as it closes each
whole-stack rejection.

### T0.1 release timings

The table records the fastest observed value for each row across three
single-pass runs and two best-of-three runs. `*` marks rows where the estimated
layer cache exceeds 1 GiB, so warm/edit use an uncached second evaluation rather
than `EvalCache`; those rows are still useful for regression tracking but do not
represent a warm editor cache. The edit row is the **first** evaluation after a
one-layer parameter bump when the cache fits (later calls are warm again); for
uncached rows it is a best-of-N uncached evaluation.

| Scene | Cold normal ms | Cold height ms | Warm normal ms | Warm height ms | Edit normal ms | Cache MiB | Peak RSS MiB |
|---|---:|---:|---:|---:|---:|---:|---:|
| s01 | 143.3 | 107.3 | 103.2 | 96.1 | 109.4 | 180 | 70 |
| s02 | 59.0 | 39.2 | 58.9* | 42.5* | 55.9* | 2020 | 70 |
| s03 | 12089.7 | 11008.5 | 12005.8* | 10977.4* | 12067.4* | 4940 | 104 |
| s04 | 104.6 | 106.6 | 102.6* | 96.8* | 107.7* | 4540 | 104 |
| s05 | 661.5 | 643.2 | 11.3 | 1.0 | 99.0 | 180 | 104 |
| s06 | 1077.6 | 784.2 | 174.1 | 32.0 | 184.3 | 640 | 481 |
| s07 | 5492.9 | 4949.5 | 5412.4* | 5000.6* | 5434.9* | 2420 | 481 |
| s08 | 171.2 | 195.4 | 136.0 | 178.7 | 155.4 | 200 | 481 |
| s09 | 1189.7 | 1173.3 | 62.0 | 27.3 | 259.9 | 160 | 481 |
| s10 | 491.0 | 177.5 | 489.7* | 182.0* | 466.4* | 6400 | 802 |

Peak RSS is process-wide, so later rows include earlier scenes' allocations.
Run one scene per process (`--scenes=s03`) for an isolated figure.

**Stability.** Best-of-three removes most intra-run noise. Between full runs the
multi-second rows still moved by 10–20 % (thermal drift and background load on a
laptop), so the plan's ±5 % target is not met for every row on this machine; the
table reports the fastest value rather than claiming reproducibility the
hardware cannot provide. Rows under ~20 ms are timer-noisy and should be read as
orders of magnitude, not exact.

### T0.3 kernel microbenchmarks

```sh
cargo bench -p ns_core --bench kernels
```

Release build, offline, fixed inputs. Throughput counts each output pixel once,
including cache-modifying variants.

| Kernel | ms | Mpix/s |
|---|---:|---:|
| edt/256/clamp | 2.46 | 26.6 |
| edt/256/repeat | 22.56 | 2.9 |
| edt/1024/clamp | 40.19 | 26.1 |
| edt/1024/repeat | 426.17 | 2.5 |
| edt/1024/clamp/outside | 40.60 | 25.8 |
| edt/2048/clamp | 201.99 | 20.8 |
| edt/2048/repeat | 2808.94 | 1.5 |
| gaussian/1024/r2 | 17.52 | 59.8 |
| gaussian/1024/r8 | 45.11 | 23.2 |
| gaussian/1024/r32 | 140.01 | 7.5 |
| scharr/1024/clamp | 13.28 | 79.0 |
| scharr/1024/repeat | 14.26 | 73.5 |
| scharr/2048/clamp | 56.37 | 74.4 |
| scharr/2048/repeat | 62.79 | 66.8 |
| blend/1024/Derivative | 16.62 | 63.1 |
| blend/1024/Rnm | 13.24 | 79.2 |
| blend/1024/Replace | 9.42 | 111.4 |
| blend/1024/masked | 10.08 | 104.0 |
| shape_region/1024/64px | 3.63 | 288.7 |
| shape_region/1024/80px | 3.42 | 306.3 |
| shape_region/1024/800px | 31.81 | 33.0 |
| fft/512 | 48.90 | 5.4 |
| fft/1024 | 450.81 | 2.3 |
| fft/2048 | 2340.98 | 1.8 |
| ao/512 | 107.61 | 2.4 |
| cavity/512 | 11.36 | 23.1 |
| curvature/512 | 4.76 | 55.1 |
| ao/1024 | 439.90 | 2.4 |
| cavity/1024 | 46.66 | 22.5 |
| curvature/1024 | 11.12 | 94.3 |

### T0.4 CPU↔GPU parity

`cargo test -p ns_app parity -- --nocapture` runs the shader through a headless
`wgpu` device and compares against `Document::evaluate()` / `evaluate_height()`.
Thresholds are ANALYTIC (mean 2.5°, p99.9 8°, max 15°, height NRMSE 0.02,
outliers 5 %), TEXTURED (height NRMSE 0.03) and STRESS (mean 4°, p99.9 9°,
outliers 12 %). Unstable silhouette/cusp pixels are excluded by a 0.05 height
step and a cos(10°) normal step; an outlier cap keeps a missing layer from
hiding behind the mask.

| Scene | Stable px | Mean ° | p99.9 ° | Max ° | Height NRMSE | Outliers >5° |
|---|---:|---:|---:|---:|---:|---:|
| demo | 60470 / 65536 | 1.207 | 6.056 | 10.114 | 0.0081 | 3.54 % |
| textured | 61676 / 65536 | 1.110 | 6.156 | 10.202 | 0.0016 | 3.41 % |
| stress-32 | 50666 / 65536 | 2.463 | 6.592 | 9.602 | 0.0051 | 8.81 % |

### T1.5 cancellation

`CancelToken` is an `Arc<AtomicBool>` checked at layer boundaries and around the
tree evaluator. `evaluate_cancellable`, `evaluate_with_cancellable`,
`evaluate_height_cancellable`, `evaluate_height_with_cancellable`,
`evaluate_auto_cancellable` and `evaluate_height_auto_cancellable` all return
`None` when the token trips. The editor cancels a worker when `eval_serial`
changes and when the GPU path succeeds, so a superseded CPU job stops instead of
running to completion. Tests cover parity with the uncancelled result, a
pre-cancelled token, and token idempotence.

### Mirror preview overlay

The live operation preview is evaluated from a temporary document, but the
canvas selection overlay was still drawn from the source document. During a
clip/merge mirror that made the old outline sit next to the previewed silhouette
and read as a second, overlapping edge. `draw_canvas_overlay` now resolves the
previewed layer for each selection (by id, then by the surviving stack slot) and
outlines that geometry while keeping the transform box and node handles on the
real editable layer. This is visual only: the committed geometry is unchanged.

A mirrored text copy also bakes to an editable path now. `TextSource` has no
flip parameter, so the old transform silently translated the glyphs instead of
reflecting them; the copy now uses the same path-baking trade-off as a mirrored
star. Operations also fail closed when a selection or group contains photo,
height-paint, normal-paint, imported-normal or fill content: those layers store
document-space grids with no affine representation, so copying them unchanged
would stack a second copy on the original instead of mirroring it.

### Validation

- `cargo test -p ns_app`: 407 passed, 4 ignored (including 3 parity tests).
- `cargo test -p ns_core`: 238 passed.
- `cargo bench -p ns_core --bench kernels` completes offline.
- `cargo check -p ns_app` and `cargo check -p ns_cli` succeed.
- `cargo fmt --all -- --check` passes.
- `cargo clippy -p ns_app --all-targets --no-deps -- -D warnings` is still
  blocked by pre-existing lints in the working tree (for example
  `chunks_exact_to_as_chunks`, `too_many_arguments`, `manual_is_multiple_of`),
  not by the Phase 0 changes.

### Still open

- **T1.1** GPU coverage for vector compounds, boolean operands, smart masks and
  group masks. The corpus now has fixed expectations for those gate reasons.
- **T1.2/T1.3** tile-binned layer lists, dirty-rect re-dispatch and incremental
  scene packing.
- **T2.x** CPU slope-space accumulators, layer/tile-parallel compositing and
  selection-aware cache eviction. The CLI's 1 GiB cache cap is a workaround for
  the missing eviction, not a fix.
- **T3.x** GPU export/depth and FFT/EDT work; **T4.x** the container v2 binary
  format. `s06` becomes practical to commit once T4.1 lands.
