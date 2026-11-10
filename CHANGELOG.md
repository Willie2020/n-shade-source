# Changelog

## Unreleased

### Added

- 3D light gizmos in the viewport while the Light tab is active or the live
  lighting preview is on. Drag a light body to move it, its aim arrow to rotate
  it, or the selected light's ring to turn it; the sun uses the same rig. The
  Light panel has a `3D light gizmos` toggle, persisted with the project.

### Fixed

- Project loading validates document size, grid invariants, mask dimensions,
  layer depth/count and the file-size cap before evaluation. Malformed or
  hand-edited `.nsproj` files now return an error instead of panicking or
  allocating unbounded memory.
- GPU parity and GPU lightmap tests fail (instead of silently passing) when no
  adapter exists and `NS_REQUIRE_GPU` is set. CI sets it.
- Native libraries are resolved from trusted absolute paths before the OS
  search path, and OIDN is loaded once per process.
- The Blender bridge pairing token comes from the OS CSPRNG; its pairing file
  is written atomically and is `0600` on Unix.
- Image imports use one decode path with explicit dimension and allocation
  limits.
- Preview tangent generation no longer panics on a mesh-format change; SCIFI
  line layout and the cage overlay have bounded/guarded degenerate inputs.

### Changed

- V2 lightmap bakes multiply area lights by the Lambertian emitter cosine.
  Area-lit surfaces get darker, most strongly at grazing angles. Legacy-pipeline
  bakes are unchanged.
- Legacy plain-RON projects carry a `// n-shade-project format=1` header.
  Files without the header still load; files with a newer format are refused.

### Security

- `unsafe`/FFI documentation is still being completed, starting with the OptiX
  module.
