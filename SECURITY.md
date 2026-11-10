# Security Policy

N-shade parses user-supplied project files, images, SVG, OBJ/FBX/COLLADA/glTF/USD
meshes and optional native denoiser libraries. Please report vulnerabilities in
those paths privately through the repository's **Security → Advisories → Report
a vulnerability** flow rather than in a public issue.

Please include the n-shade version, the operating system, the smallest input
that reproduces the problem, and whether a GPU or the Blender bridge was
involved.

## Scope

- Memory-safety issues in `unsafe`/FFI code (`ns_bake/src/optix.rs`,
  `ns_bake/src/oidn.rs`, `ns_app/src/pen_input.rs`).
- Denial of service through oversized or malformed project/image/mesh files.
- Path traversal or unexpected file writes while loading or saving projects.
- Blender bridge authentication or frame-parsing issues.

## Not in scope

- Crashes that require a locally modified build or a debugger.
- GPU driver bugs; report those to the vendor.
- Issues that require the attacker to already control the user's account.
