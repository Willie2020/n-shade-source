# Benchmark corpus (T0.1)

`bench/scenes/` is generated, not hand-edited. Layer ids are stabilised so a
regeneration only changes the scene contents you actually changed.

```sh
# Regenerate everything and record timings.
cargo run --profile profiling -p ns_cli -- bench --repeats=3

# Generate without evaluating.
cargo run -p ns_cli -- bench --no-run

# One scene, useful for an isolated peak-RSS figure.
cargo run --profile profiling -p ns_cli -- bench --scenes=s03 --repeats=3
```

| Scene | Source |
|---|---|
| `s01_8shapes_4masks_1k` | generated: shapes, painted masks |
| `s02_101layers_1k` | `ns_core::stress_document(1024, 100)` |
| `s03_247_showcase` | copy of `showcase/scifi_panel_detailed.nsproj` |
| `s04_honeycomb_compound` | generated: 225-cell vector compound |
| `s05_paths_8x5000` | generated: eight 5000-node paths |
| `s06_photo_paint_2k` | generated on demand (RON grids are hundreds of MiB) |
| `s07_nested_groups` | generated: nested groups with painted group masks |
| `s08_smart_masks` | generated: slope/curvature/noise generators |
| `s09_tileable_repeat` | generated: `Wrap::Repeat` with edge-crossing shapes |
| `s10_4k_20layers` | `ns_core::stress_document(4096, 19)` |

`s03` is a generated copy of a tracked showcase, and `s06` is regenerated in
memory unless `--save-raster` is passed, so both are gitignored. Timings are
recorded in `docs/perf-baseline.md`; the benchmark writes one Markdown row per
scene to stdout.
