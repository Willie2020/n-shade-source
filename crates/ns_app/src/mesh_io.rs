//! Native mesh readers for FBX, OBJ, DAE and USD files.
//!
//! glTF/GLB files are loaded through Bevy's asset server, but the formats
//! below have no built-in loader. This module turns them into a small,
//! engine-agnostic [`RawMesh`] representation that the import pipeline can
//! convert into Bevy meshes. All readers preserve per-corner UVs where the
//! format allows it, which keeps UDIM analysis and normal mapping correct.

use std::collections::HashMap;
use std::path::Path;

use glam::{Mat3, Mat4, Quat, Vec2, Vec3};

/// Mesh formats understood by the native import path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeshFormat {
    Obj,
    Dae,
    Fbx,
    Usd,
    /// N-shade's own portable mesh payload.
    NsMesh,
}

/// Classify a path by extension. Returns `None` for unsupported files and for
/// glTF/GLB, which take the asset-server path.
pub fn mesh_format(path: &Path) -> Option<MeshFormat> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "obj" => Some(MeshFormat::Obj),
        "dae" => Some(MeshFormat::Dae),
        "fbx" => Some(MeshFormat::Fbx),
        "usd" | "usda" | "usdc" | "usdz" => Some(MeshFormat::Usd),
        "nsmesh" => Some(MeshFormat::NsMesh),
        _ => None,
    }
}

pub fn is_gltf_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "glb" | "gltf"))
}

/// True when a glTF/GLB declares at least one scene.
///
/// Bevy labels scene assets `Scene0`, `Scene1`, … and fails the load when the
/// file has no `scenes` array at all. Many exporters write mesh-only GLBs, so
/// the importer checks this up front and converts those through the same
/// native mesh path as OBJ/FBX instead of waiting for a scene that will never
/// exist. A file that cannot be parsed is reported as having a scene so Bevy's
/// loader produces the error message.
pub fn gltf_has_scene(path: &Path) -> bool {
    gltf::Gltf::open(path)
        .map(|gltf| gltf.document.scenes().len() > 0)
        .unwrap_or(true)
}

pub fn is_mesh_path(path: &Path) -> bool {
    is_gltf_path(path) || mesh_format(path).is_some()
}

/// A triangle mesh in engine-agnostic form. Normals are smoothed when the
/// source file does not provide them, so downstream code can always rely on
/// them for lighting and tangent generation.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RawMesh {
    pub name: String,
    /// Object or instance name from the file hierarchy. Per-object lightmap
    /// separation groups by this and falls back to [`Self::name`].
    pub object: String,
    /// Material bound to this primitive, when the format exposes one. Per-
    /// material separation falls back to the object name when it is missing.
    pub material: Option<String>,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Option<Vec<[f32; 2]>>,
    pub indices: Vec<u32>,
}

impl RawMesh {
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// Name used by per-object bake separation.
    pub fn object_name(&self) -> &str {
        if self.object.is_empty() {
            &self.name
        } else {
            &self.object
        }
    }
}

/// Load every renderable mesh in a native file.
pub fn load_native(path: &Path) -> Result<Vec<RawMesh>, String> {
    let format = mesh_format(path)
        .ok_or_else(|| format!("{} is not a supported mesh file", path.display()))?;
    let meshes = match format {
        MeshFormat::Obj => load_obj(path)?,
        MeshFormat::Dae => load_dae(path)?,
        MeshFormat::Fbx => load_fbx(path)?,
        MeshFormat::Usd => load_usd(path)?,
        MeshFormat::NsMesh => load_nsmesh(path)?,
    };
    let meshes: Vec<RawMesh> = meshes
        .into_iter()
        .filter(|mesh| mesh.positions.len() >= 3 && mesh.indices.len() >= 3)
        .collect();
    if meshes.is_empty() {
        return Err(format!("{} contains no triangle meshes", path.display()));
    }
    Ok(meshes)
}

/// Load any supported mesh file, including glTF/GLB. glTF files are parsed
/// with the same CPU-side mesh representation used by the native readers so
/// baking can see raw file coordinates instead of the normalized preview
/// scene.
pub fn load_any(path: &Path) -> Result<Vec<RawMesh>, String> {
    if is_gltf_path(path) {
        load_gltf(path)
    } else {
        load_native(path)
    }
}

/// Read the UV-origin flag from a `.nsmesh` payload without rebuilding the
/// whole import. `false` means the glTF top-left convention.
pub fn nsmesh_uv_bottom_left(path: &Path) -> bool {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| ns_core::mesh_asset::MeshAsset::decode(&bytes).ok())
        .map(|mesh| mesh.uv_bottom_left)
        .unwrap_or(false)
}

fn load_nsmesh(path: &Path) -> Result<Vec<RawMesh>, String> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    let asset = ns_core::mesh_asset::MeshAsset::decode(&bytes)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    Ok(mesh_asset_to_raw(asset))
}

/// Split a portable mesh into one [`RawMesh`] per primitive group so object
/// and material bake separation keeps working.
fn mesh_asset_to_raw(asset: ns_core::mesh_asset::MeshAsset) -> Vec<RawMesh> {
    let ns_core::mesh_asset::MeshAsset {
        name,
        positions,
        normals,
        uvs,
        indices,
        groups,
        uv_bottom_left: _,
    } = asset;
    let has_uvs = uvs.is_some();
    if groups.is_empty() {
        return vec![RawMesh {
            name: name.clone(),
            object: name.clone(),
            material: None,
            positions,
            normals,
            uvs,
            indices,
        }];
    }
    groups
        .into_iter()
        .map(|group| {
            let start = group.first_index as usize;
            let end = (start + group.index_count as usize).min(indices.len());
            RawMesh {
                name: if group.name.is_empty() {
                    name.clone()
                } else {
                    group.name.clone()
                },
                object: group.object.clone(),
                material: group.material.clone(),
                positions: positions.clone(),
                normals: normals.clone(),
                uvs: if has_uvs { uvs.clone() } else { None },
                indices: indices[start..end].to_vec(),
            }
        })
        .collect()
}

fn load_gltf(path: &Path) -> Result<Vec<RawMesh>, String> {
    let gltf = gltf::Gltf::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let document = gltf.document;
    let buffers = gltf::import_buffers(&document, path.parent(), gltf.blob)
        .map_err(|error| format!("{}: {error}", path.display()))?;

    let mut has_parent = vec![false; document.nodes().len()];
    for node in document.nodes() {
        for child in node.children() {
            has_parent[child.index()] = true;
        }
    }

    let mut meshes = Vec::new();
    for node in document.nodes() {
        if !has_parent[node.index()] {
            walk_gltf_node(node, Mat4::IDENTITY, &buffers, &mut meshes);
        }
    }
    let meshes: Vec<RawMesh> = meshes
        .into_iter()
        .filter(|mesh| mesh.positions.len() >= 3 && mesh.indices.len() >= 3)
        .collect();
    if meshes.is_empty() {
        return Err(format!("{} contains no triangle meshes", path.display()));
    }
    Ok(meshes)
}

fn walk_gltf_node(
    node: gltf::Node,
    parent: Mat4,
    buffers: &[gltf::buffer::Data],
    meshes: &mut Vec<RawMesh>,
) {
    let local = Mat4::from_cols_array_2d(&node.transform().matrix());
    let world = parent * local;
    let normal_matrix = Mat3::from_mat4(world);
    let normal_matrix = if normal_matrix.determinant().abs() > 1e-12 {
        normal_matrix.inverse().transpose()
    } else {
        Mat3::IDENTITY
    };

    if let Some(mesh) = node.mesh() {
        let name = mesh.name().unwrap_or("mesh");
        let object = node.name().unwrap_or(name);
        for primitive in mesh.primitives() {
            if let Some(raw) =
                gltf_primitive(&primitive, world, normal_matrix, name, object, buffers)
            {
                meshes.push(raw);
            }
        }
    }
    for child in node.children() {
        walk_gltf_node(child, world, buffers, meshes);
    }
}

fn gltf_primitive(
    primitive: &gltf::Primitive,
    world: Mat4,
    normal_matrix: Mat3,
    name: &str,
    object: &str,
    buffers: &[gltf::buffer::Data],
) -> Option<RawMesh> {
    let reader = primitive.reader(|buffer| Some(&buffers[buffer.index()]));
    let positions: Vec<[f32; 3]> = reader
        .read_positions()?
        .map(|position| {
            world
                .transform_point3(Vec3::from_array(position))
                .to_array()
        })
        .collect();
    if positions.len() < 3 {
        return None;
    }

    let mut indices: Vec<u32> = reader
        .read_indices()
        .map(|indices| indices.into_u32().collect())
        .unwrap_or_else(|| (0..positions.len() as u32).collect());
    indices = match primitive.mode() {
        gltf::mesh::Mode::Triangles => indices,
        gltf::mesh::Mode::TriangleStrip => triangle_strip(&indices),
        gltf::mesh::Mode::TriangleFan => triangle_fan(&indices),
        _ => return None,
    };

    let mut filtered = Vec::with_capacity(indices.len());
    for triangle in indices.chunks_exact(3) {
        if triangle
            .iter()
            .all(|index| (*index as usize) < positions.len())
        {
            filtered.extend_from_slice(triangle);
        }
    }
    if filtered.len() < 3 {
        return None;
    }
    indices = filtered;

    let normals: Option<Vec<[f32; 3]>> = reader.read_normals().map(|normals| {
        normals
            .map(|normal| {
                let normal = normal_matrix * Vec3::from_array(normal);
                if normal.length_squared() > 1e-20 {
                    normal.normalize().to_array()
                } else {
                    [0.0, 1.0, 0.0]
                }
            })
            .collect()
    });
    let normals = match normals {
        Some(normals) if normals.len() == positions.len() => normals,
        _ => smooth_normals(&positions, &indices),
    };
    let uvs = reader
        .read_tex_coords(0)
        .map(|uvs| uvs.into_f32().collect::<Vec<_>>())
        .filter(|uvs| uvs.len() == positions.len());

    let material = primitive.material();
    let material_name = material
        .name()
        .map(str::to_string)
        .or_else(|| material.index().map(|index| format!("Material {index}")));

    Some(RawMesh {
        name: name.to_string(),
        object: object.to_string(),
        material: material_name,
        positions,
        normals,
        uvs,
        indices,
    })
}

fn triangle_strip(indices: &[u32]) -> Vec<u32> {
    let mut output = Vec::with_capacity(indices.len().saturating_sub(2) * 3);
    for index in 0..indices.len().saturating_sub(2) {
        if index % 2 == 0 {
            output.extend_from_slice(&[indices[index], indices[index + 1], indices[index + 2]]);
        } else {
            output.extend_from_slice(&[indices[index + 1], indices[index], indices[index + 2]]);
        }
    }
    output
}

fn triangle_fan(indices: &[u32]) -> Vec<u32> {
    let mut output = Vec::with_capacity(indices.len().saturating_sub(2) * 3);
    for index in 1..indices.len().saturating_sub(1) {
        output.extend_from_slice(&[indices[0], indices[index], indices[index + 1]]);
    }
    output
}

/// Deduplicates shared corners while collecting per-corner attributes. FBX,
/// DAE and USD all index position, normal and UV independently, so a single
/// output vertex is identified by the tuple of its source values.
struct MeshBuilder {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    indices: Vec<u32>,
    has_normals: bool,
    has_uvs: bool,
    normals_valid: bool,
    uvs_valid: bool,
    lookup: HashMap<CornerKey, u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct CornerKey {
    position: [u32; 3],
    normal: [u32; 3],
    uv: [u32; 2],
}

impl MeshBuilder {
    fn new(has_normals: bool, has_uvs: bool) -> Self {
        Self {
            positions: Vec::new(),
            normals: Vec::new(),
            uvs: Vec::new(),
            indices: Vec::new(),
            has_normals,
            has_uvs,
            normals_valid: true,
            uvs_valid: true,
            lookup: HashMap::new(),
        }
    }

    fn push(&mut self, position: Vec3, normal: Option<Vec3>, uv: Option<Vec2>) -> u32 {
        if self.has_normals && normal.is_none() {
            self.normals_valid = false;
        }
        if self.has_uvs && uv.is_none() {
            self.uvs_valid = false;
        }
        let normal = normal.unwrap_or(Vec3::Y);
        let uv = uv.unwrap_or(Vec2::ZERO);
        let key = CornerKey {
            position: bits3(position),
            normal: if self.has_normals {
                bits3(normal)
            } else {
                [0; 3]
            },
            uv: if self.has_uvs { bits2(uv) } else { [0; 2] },
        };
        if let Some(index) = self.lookup.get(&key) {
            return *index;
        }
        let index = self.positions.len() as u32;
        self.positions.push(position.to_array());
        if self.has_normals {
            self.normals.push(normal.to_array());
        }
        if self.has_uvs {
            self.uvs.push(uv.to_array());
        }
        self.lookup.insert(key, index);
        index
    }

    fn triangle(&mut self, a: u32, b: u32, c: u32) {
        if a != b && b != c && a != c {
            self.indices.extend_from_slice(&[a, b, c]);
        }
    }

    fn finish(
        self,
        name: impl Into<String>,
        object: impl Into<String>,
        material: Option<String>,
    ) -> Option<RawMesh> {
        if self.indices.len() < 3 {
            return None;
        }
        let normals =
            if self.has_normals && self.normals_valid && self.normals.len() == self.positions.len()
            {
                self.normals
            } else {
                smooth_normals(&self.positions, &self.indices)
            };
        let uvs = (self.has_uvs && self.uvs_valid && self.uvs.len() == self.positions.len())
            .then_some(self.uvs);
        Some(RawMesh {
            name: name.into(),
            object: object.into(),
            material,
            positions: self.positions,
            normals,
            uvs,
            indices: self.indices,
        })
    }
}

fn bits(value: f32) -> u32 {
    // Collapse signed zero so equivalent corners deduplicate.
    if value == 0.0 {
        0.0f32.to_bits()
    } else {
        value.to_bits()
    }
}

fn bits3(value: Vec3) -> [u32; 3] {
    [bits(value.x), bits(value.y), bits(value.z)]
}

fn bits2(value: Vec2) -> [u32; 2] {
    [bits(value.x), bits(value.y)]
}

/// Area-weighted smooth normals for sources that do not store them. Vertices
/// split by UV seams are merged by position so the seams stay smooth.
pub(crate) fn smooth_normals(positions: &[[f32; 3]], indices: &[u32]) -> Vec<[f32; 3]> {
    let mut sums: HashMap<[u32; 3], Vec3> = HashMap::new();
    for triangle in indices.chunks_exact(3) {
        let Some(&a) = positions.get(triangle[0] as usize) else {
            continue;
        };
        let Some(&b) = positions.get(triangle[1] as usize) else {
            continue;
        };
        let Some(&c) = positions.get(triangle[2] as usize) else {
            continue;
        };
        let normal = (Vec3::from_array(b) - Vec3::from_array(a))
            .cross(Vec3::from_array(c) - Vec3::from_array(a));
        for &index in triangle {
            let Some(position) = positions.get(index as usize) else {
                continue;
            };
            *sums.entry(bits3(Vec3::from_array(*position))).or_default() += normal;
        }
    }
    positions
        .iter()
        .map(|position| {
            let normal = sums
                .get(&bits3(Vec3::from_array(*position)))
                .copied()
                .unwrap_or(Vec3::Y);
            if normal.length_squared() > 1e-20 {
                normal.normalize().to_array()
            } else {
                [0.0, 1.0, 0.0]
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// OBJ
// ---------------------------------------------------------------------------

fn load_obj(path: &Path) -> Result<Vec<RawMesh>, String> {
    let (models, materials) = tobj::load_obj(path, &tobj::GPU_LOAD_OPTIONS)
        .map_err(|error| format!("OBJ parse error: {error}"))?;
    // A missing or unreadable MTL file must not reject the geometry; OBJ meshes
    // are still bakeable without material names.
    let materials = materials.unwrap_or_default();

    let mut meshes = Vec::with_capacity(models.len());
    for model in models {
        let obj = model.mesh;
        let vertex_count = obj.positions.len() / 3;
        if vertex_count == 0 || obj.indices.is_empty() {
            continue;
        }
        let positions: Vec<[f32; 3]> = obj
            .positions
            .chunks_exact(3)
            .map(|position| [position[0], position[1], position[2]])
            .collect();
        // With `single_index`, normals and UVs are aligned with positions.
        let normals = (obj.normals.len() == positions.len() * 3).then(|| {
            obj.normals
                .chunks_exact(3)
                .map(|normal| [normal[0], normal[1], normal[2]])
                .collect::<Vec<_>>()
        });
        let uvs = (obj.texcoords.len() == positions.len() * 2).then(|| {
            obj.texcoords
                .chunks_exact(2)
                .map(|uv| [uv[0], uv[1]])
                .collect::<Vec<_>>()
        });
        let normals = normals.unwrap_or_else(|| smooth_normals(&positions, &obj.indices));
        let material = obj
            .material_id
            .and_then(|id| materials.get(id))
            .map(|material| material.name.clone());
        let object = model.name.clone();
        meshes.push(RawMesh {
            name: model.name,
            object,
            material,
            positions,
            normals,
            uvs,
            indices: obj.indices,
        });
    }
    Ok(meshes)
}

// ---------------------------------------------------------------------------
// FBX
// ---------------------------------------------------------------------------

fn load_fbx(path: &Path) -> Result<Vec<RawMesh>, String> {
    let file_name = path.to_string_lossy();
    let scene = ufbx::load_file(
        &file_name,
        ufbx::LoadOpts {
            generate_missing_normals: true,
            ..Default::default()
        },
    )
    .map_err(|error| format!("FBX parse error: {}", error.description))?;

    let mut meshes = Vec::new();
    for node in &scene.nodes {
        if !node.visible {
            continue;
        }
        let Some(mesh) = node.mesh.as_ref() else {
            continue;
        };
        let world = ufbx_matrix_to_glam(node.geometry_to_world);
        let normal_matrix = Mat3::from_mat4(world).inverse().transpose();
        let has_normals = mesh.vertex_normal.exists;
        let has_uvs = mesh.vertex_uv.exists;
        let object = if node.element.name.is_empty() {
            mesh.element.name.to_string()
        } else {
            node.element.name.to_string()
        };
        // A node can mix several materials. Split them into one RawMesh each so
        // per-material lightmap separation has clean, non-overlapping groups.
        let mut builders: Vec<(Option<usize>, Option<String>, MeshBuilder)> = Vec::new();
        let mut corners = Vec::new();

        for (face_index, face) in mesh.faces.iter().enumerate() {
            if face.num_indices < 3 {
                continue;
            }
            let material_index = mesh
                .face_material
                .as_ref()
                .get(face_index)
                .copied()
                .map(|index| index as usize);
            let material = material_index
                .and_then(|index| node.materials.as_ref().get(index))
                .map(|material| material.element.name.to_string())
                .filter(|name| !name.is_empty());
            let builder_index = match builders
                .iter()
                .position(|(index, name, _)| *index == material_index && *name == material)
            {
                Some(index) => index,
                None => {
                    builders.push((
                        material_index,
                        material.clone(),
                        MeshBuilder::new(has_normals, has_uvs),
                    ));
                    builders.len() - 1
                }
            };
            let builder = &mut builders[builder_index].2;

            let needed = (face.num_indices as usize).saturating_sub(2) * 3;
            corners.resize(needed.max(3), 0);
            // Returns the number of triangles written, not the index count.
            let written = mesh.triangulate_face(&mut corners, *face) as usize * 3;
            for triangle in corners[..written.min(corners.len())].chunks_exact(3) {
                let mut indices = [0u32; 3];
                for (slot, &corner) in indices.iter_mut().zip(triangle) {
                    let corner = corner as usize;
                    if corner >= mesh.num_indices {
                        continue;
                    }
                    let position = world.transform_point3(ufbx_vec3(mesh.vertex_position[corner]));
                    let normal = has_normals.then(|| {
                        (normal_matrix * ufbx_vec3(mesh.vertex_normal[corner])).normalize_or_zero()
                    });
                    let uv = has_uvs.then(|| {
                        let uv = mesh.vertex_uv[corner];
                        Vec2::new(uv.x as f32, uv.y as f32)
                    });
                    *slot = builder.push(position, normal, uv);
                }
                builder.triangle(indices[0], indices[1], indices[2]);
            }
        }

        for (_, material, builder) in builders {
            if let Some(raw) =
                builder.finish(mesh.element.name.to_string(), object.clone(), material)
            {
                meshes.push(raw);
            }
        }
    }
    Ok(meshes)
}

fn ufbx_vec3(value: ufbx::Vec3) -> Vec3 {
    Vec3::new(value.x as f32, value.y as f32, value.z as f32)
}

fn ufbx_matrix_to_glam(matrix: ufbx::Matrix) -> Mat4 {
    let column0 = Vec3::new(matrix.m00 as f32, matrix.m10 as f32, matrix.m20 as f32);
    let column1 = Vec3::new(matrix.m01 as f32, matrix.m11 as f32, matrix.m21 as f32);
    let column2 = Vec3::new(matrix.m02 as f32, matrix.m12 as f32, matrix.m22 as f32);
    let translation = Vec3::new(matrix.m03 as f32, matrix.m13 as f32, matrix.m23 as f32);
    Mat4::from_cols(
        column0.extend(0.0),
        column1.extend(0.0),
        column2.extend(0.0),
        translation.extend(1.0),
    )
}

// ---------------------------------------------------------------------------
// COLLADA (DAE)
// ---------------------------------------------------------------------------

fn load_dae(path: &Path) -> Result<Vec<RawMesh>, String> {
    use dae_parser::Document;

    let document =
        Document::from_file(path).map_err(|error| format!("DAE parse error: {error:?}"))?;
    let sources = document.local_map::<dae_parser::Source>().ok();
    let vertices = document.local_map::<dae_parser::Vertices>().ok();
    let geometries = document.local_map::<dae_parser::Geometry>().ok();
    let controllers = document.local_map::<dae_parser::Controller>().ok();
    let visual_scenes = document.local_map::<dae_parser::VisualScene>().ok();

    let mut meshes = Vec::new();
    // COLLADA is right-handed but may declare X-up or Z-up. Rotate into the
    // Y-up convention used by the viewer and the other importers.
    let up = match document.asset.up_axis {
        dae_parser::UpAxis::ZUp => Mat4::from_rotation_x(-std::f32::consts::FRAC_PI_2),
        dae_parser::UpAxis::XUp => Mat4::from_rotation_z(std::f32::consts::FRAC_PI_2),
        dae_parser::UpAxis::YUp => Mat4::IDENTITY,
    };
    let visual_scene = document
        .scene
        .as_ref()
        .and_then(|scene| scene.instance_visual_scene.as_ref())
        .and_then(|instance| {
            visual_scenes
                .as_ref()
                .and_then(|map| map.get(&instance.url))
        });

    if let Some(scene) = visual_scene {
        for node in &scene.nodes {
            walk_dae_node(
                node,
                up,
                &sources,
                &vertices,
                &geometries,
                &controllers,
                &mut meshes,
            );
        }
    } else {
        // Files without an instantiated visual scene still carry geometry;
        // import it in file order so they are not rejected outright.
        for geometry in document.iter::<dae_parser::Geometry>() {
            if let dae_parser::GeometryElement::Mesh(mesh) = &geometry.element {
                for primitive in &mesh.elements {
                    if let Some(raw) = dae_primitive(
                        primitive,
                        up,
                        &sources,
                        &vertices,
                        geometry.name.as_deref().unwrap_or("Collada mesh"),
                    ) {
                        meshes.push(raw);
                    }
                }
            }
        }
    }
    Ok(meshes)
}

fn walk_dae_node(
    node: &dae_parser::Node,
    parent: Mat4,
    sources: &Option<dae_parser::LocalMap<'_, dae_parser::Source>>,
    vertices: &Option<dae_parser::LocalMap<'_, dae_parser::Vertices>>,
    geometries: &Option<dae_parser::LocalMap<'_, dae_parser::Geometry>>,
    controllers: &Option<dae_parser::LocalMap<'_, dae_parser::Controller>>,
    meshes: &mut Vec<RawMesh>,
) {
    let mut local = Mat4::IDENTITY;
    for transform in &node.transforms {
        local *= dae_transform(transform);
    }
    let world = parent * local;

    for instance in &node.instance_geometry {
        let Some(geometry) = geometries.as_ref().and_then(|map| map.get(&instance.url)) else {
            continue;
        };
        let dae_parser::GeometryElement::Mesh(mesh) = &geometry.element else {
            continue;
        };
        let name = geometry
            .name
            .as_deref()
            .or(geometry.id.as_deref())
            .unwrap_or("Collada mesh");
        for primitive in &mesh.elements {
            if let Some(raw) = dae_primitive(primitive, world, sources, vertices, name) {
                meshes.push(raw);
            }
        }
    }

    // Skinned meshes reference a controller, whose skin points back at the
    // renderable mesh. Import the bind-pose geometry; animation is not played.
    for instance in &node.instance_controller {
        let Some(controller) = controllers.as_ref().and_then(|map| map.get(&instance.url)) else {
            continue;
        };
        let dae_parser::ControlElement::Skin(skin) = &controller.element else {
            continue;
        };
        let Some(geometry) = geometries
            .as_ref()
            .and_then(|map| map.get_raw(&skin.source.val))
        else {
            continue;
        };
        let dae_parser::GeometryElement::Mesh(mesh) = &geometry.element else {
            continue;
        };
        let name = controller
            .name
            .as_deref()
            .or(geometry.name.as_deref())
            .unwrap_or("Collada skinned mesh");
        for primitive in &mesh.elements {
            if let Some(raw) = dae_primitive(primitive, world, sources, vertices, name) {
                meshes.push(raw);
            }
        }
    }

    for child in &node.children {
        walk_dae_node(
            child,
            world,
            sources,
            vertices,
            geometries,
            controllers,
            meshes,
        );
    }
}

fn dae_transform(transform: &dae_parser::Transform) -> Mat4 {
    use dae_parser::Transform;
    match transform {
        Transform::Matrix(matrix) => Mat4::from_cols_array(&matrix.0),
        Transform::Translate(translate) => {
            Mat4::from_translation(Vec3::new(translate.0[0], translate.0[1], translate.0[2]))
        }
        Transform::Scale(scale) => Mat4::from_scale(Vec3::new(scale.0[0], scale.0[1], scale.0[2])),
        Transform::Rotate(rotate) => {
            let axis = Vec3::new(rotate.0[0], rotate.0[1], rotate.0[2]);
            if axis.length_squared() > 1e-20 {
                Mat4::from_quat(Quat::from_axis_angle(
                    axis.normalize(),
                    rotate.0[3].to_radians(),
                ))
            } else {
                Mat4::IDENTITY
            }
        }
        // Skew and look-at transforms are uncommon in static mesh exports;
        // keeping the rest of the hierarchy intact is more useful than failing.
        Transform::Skew(_) | Transform::LookAt(_) => Mat4::IDENTITY,
    }
}

struct DaeReader<'a> {
    source: &'a dae_parser::Source,
}

fn dae_primitive(
    primitive: &dae_parser::Primitive,
    world: Mat4,
    sources: &Option<dae_parser::LocalMap<'_, dae_parser::Source>>,
    vertices: &Option<dae_parser::LocalMap<'_, dae_parser::Vertices>>,
    name: &str,
) -> Option<RawMesh> {
    use dae_parser::{Primitive, Semantic};

    let sources = sources.as_ref()?;
    let material = match primitive {
        Primitive::Triangles(triangles) => triangles.material.clone(),
        Primitive::PolyList(polylist) => polylist.material.clone(),
        _ => None,
    };
    let (inputs, corner_data, triangle_corners) = match primitive {
        Primitive::Triangles(triangles) => {
            let data = triangles.data.prim.as_deref()?;
            let stride = triangles.inputs.stride;
            if stride == 0 || data.len() % (stride * 3) != 0 {
                return None;
            }
            let triangle_count = data.len() / (stride * 3);
            let corners = (0..triangle_count)
                .map(|triangle| {
                    let base = (triangle * 3) as u32;
                    [base, base + 1, base + 2]
                })
                .collect();
            (&triangles.inputs, data, corners)
        }
        Primitive::PolyList(polylist) => {
            let data = &polylist.data;
            let stride = polylist.inputs.stride;
            let corners: usize = data.vcount.iter().map(|count| *count as usize).sum();
            if stride == 0 || corners * stride != data.prim.len() {
                return None;
            }
            let corners = poly_triangles(&data.vcount);
            (&polylist.inputs, &data.prim[..], corners)
        }
        _ => return None,
    };

    let stride = inputs.stride;
    let vertex_input = inputs
        .iter()
        .find(|input| input.semantic == Semantic::Vertex)?;
    let vertex_set = vertices.as_ref()?.get_raw(&vertex_input.source)?;
    let position_offset = vertex_input.offset as usize;
    let position_source = vertex_set
        .inputs
        .get(vertex_set.position)
        .and_then(|input| sources.get_raw(&input.source))?;
    let position_reader = DaeReader {
        source: position_source,
    };

    let find_reader = |semantic: Semantic| -> Option<(usize, DaeReader)> {
        if let Some(input) = inputs.iter().find(|input| input.semantic == semantic) {
            return sources
                .get_raw(&input.source)
                .map(|source| (input.offset as usize, DaeReader { source }));
        }
        if let Some(input) = vertex_set
            .inputs
            .iter()
            .find(|input| input.semantic == semantic)
        {
            return sources
                .get_raw(&input.source)
                .map(|source| (position_offset, DaeReader { source }));
        }
        None
    };
    let normal_reader = find_reader(Semantic::Normal);
    let uv_reader = find_reader(Semantic::TexCoord).or_else(|| find_reader(Semantic::UV));

    let mut builder = MeshBuilder::new(normal_reader.is_some(), uv_reader.is_some());
    let normal_matrix = Mat3::from_mat4(world).inverse().transpose();

    for triangle in triangle_corners {
        let mut output = [0u32; 3];
        for (slot, corner) in output.iter_mut().zip(triangle) {
            let base = corner as usize * stride;
            let Some(position_index) = corner_data.get(base + position_offset) else {
                continue;
            };
            let Some(position) = position_reader.read_vec3(*position_index as usize) else {
                continue;
            };
            let normal = normal_reader.as_ref().and_then(|(offset, reader)| {
                let index = *corner_data.get(base + *offset)?;
                reader
                    .read_vec3(index as usize)
                    .map(|normal| (normal_matrix * normal).normalize_or_zero())
            });
            let uv = uv_reader.as_ref().and_then(|(offset, reader)| {
                let index = *corner_data.get(base + *offset)?;
                reader.read_vec2(index as usize)
            });
            *slot = builder.push(world.transform_point3(position), normal, uv);
        }
        builder.triangle(output[0], output[1], output[2]);
    }

    builder.finish(name, name, material)
}

/// Fan-triangulate a polylist, returning corner numbers (not raw indices).
fn poly_triangles(vcount: &[u32]) -> Vec<[u32; 3]> {
    let mut triangles = Vec::new();
    let mut corner = 0u32;
    for &count in vcount {
        for i in 1..count.saturating_sub(1) {
            triangles.push([corner, corner + i, corner + i + 1]);
        }
        corner += count;
    }
    triangles
}

impl DaeReader<'_> {
    fn read_vec3(&self, index: usize) -> Option<Vec3> {
        let array = match self.source.array.as_ref()? {
            dae_parser::ArrayElement::Float(array) => array,
            _ => return None,
        };
        let base = self.source.accessor.offset + index * self.source.accessor.stride;
        let x = *array.get(base)?;
        let y = *array.get(base + 1)?;
        let z = *array.get(base + 2)?;
        Some(Vec3::new(x, y, z))
    }

    fn read_vec2(&self, index: usize) -> Option<Vec2> {
        let array = match self.source.array.as_ref()? {
            dae_parser::ArrayElement::Float(array) => array,
            _ => return None,
        };
        let base = self.source.accessor.offset + index * self.source.accessor.stride;
        let u = *array.get(base)?;
        let v = *array.get(base + 1)?;
        Some(Vec2::new(u, v))
    }
}

// ---------------------------------------------------------------------------
// USD
// ---------------------------------------------------------------------------

fn load_usd(path: &Path) -> Result<Vec<RawMesh>, String> {
    let file_name = path.to_string_lossy();
    let stage = openusd::usd::Stage::open(&file_name)
        .map_err(|error| format!("USD parse error: {error}"))?;

    let roots = stage
        .root_prims()
        .map_err(|error| format!("USD root error: {error}"))?;
    let root_names: Vec<String> = if roots.is_empty() {
        stage
            .default_prim()
            .map(|token| vec![token.as_str().to_string()])
            .unwrap_or_default()
    } else {
        roots
            .iter()
            .map(|token| token.as_str().to_string())
            .collect()
    };
    let mut meshes = Vec::new();
    // USD files declare Y-up or Z-up in stage metadata. Rotate Z-up scenes
    // into the viewer's Y-up convention.
    let up = match stage.stage_metadata("upAxis").ok().flatten() {
        Some(openusd::sdf::Value::Token(token)) if token.as_str().eq_ignore_ascii_case("Z") => {
            Mat4::from_rotation_x(-std::f32::consts::FRAC_PI_2)
        }
        Some(openusd::sdf::Value::String(value)) if value.eq_ignore_ascii_case("Z") => {
            Mat4::from_rotation_x(-std::f32::consts::FRAC_PI_2)
        }
        _ => Mat4::IDENTITY,
    };
    for root in root_names {
        let Ok(root_path) = openusd::sdf::Path::new(&format!("/{}", root)) else {
            continue;
        };
        let prim = stage.prim(root_path);
        walk_usd_prim(&stage, &prim, up, &mut meshes);
    }
    Ok(meshes)
}

fn walk_usd_prim(
    stage: &openusd::usd::Stage,
    prim: &openusd::usd::Prim,
    parent: Mat4,
    meshes: &mut Vec<RawMesh>,
) {
    use openusd::schemas::geom::{Mesh as UsdMesh, Xform, Xformable};

    let world = if let Ok(Some(xform)) = Xform::get(stage, prim.path().clone()) {
        match xform.local_to_parent_transform(0.0) {
            Ok(matrix) => parent * usd_matrix(matrix),
            Err(_) => parent,
        }
    } else if let Ok(Some(mesh)) = UsdMesh::get(stage, prim.path().clone()) {
        match mesh.local_to_parent_transform(0.0) {
            Ok(matrix) => parent * usd_matrix(matrix),
            Err(_) => parent,
        }
    } else {
        parent
    };

    if let Ok(Some(mesh)) = UsdMesh::get(stage, prim.path().clone()) {
        let material = usd_material_name(stage, prim);
        if let Some(raw) = usd_mesh(&mesh, world, prim, material) {
            meshes.push(raw);
        }
    }

    let Ok(children) = prim.children() else {
        return;
    };
    for child in children {
        walk_usd_prim(stage, &child, world, meshes);
    }
}

fn usd_mesh(
    mesh: &openusd::schemas::geom::Mesh,
    world: Mat4,
    prim: &openusd::usd::Prim,
    material: Option<String>,
) -> Option<RawMesh> {
    use openusd::schemas::geom::PointBased;

    let points = read_vec3_attr(&mesh.points_attr())?;
    let counts = read_int_attr(&mesh.face_vertex_counts_attr())?;
    let indices = read_int_attr(&mesh.face_vertex_indices_attr())?;
    if points.is_empty() || indices.is_empty() {
        return None;
    }

    let normals = read_vec3_attr(&mesh.normals_attr());
    let normal_interpolation = interpolation(&mesh.normals_attr());
    let normal_indices = primvar_indices(&mesh.normals_attr());
    let (uvs, uv_interpolation, uv_indices) = find_uv_primvar(prim);

    let mut builder = MeshBuilder::new(normals.is_some(), uvs.is_some());
    let normal_matrix = Mat3::from_mat4(world).inverse().transpose();
    let mut corner = 0usize;

    for (face, count) in counts.iter().enumerate() {
        let count = (*count).max(0) as usize;
        if count < 3 || corner + count > indices.len() {
            corner += count;
            continue;
        }
        let face_corners = &indices[corner..corner + count];
        for i in 1..count - 1 {
            // Keep the original polygon corner numbers: primvar indices are
            // authored per polygon corner, not per triangulated corner.
            let triangle_corners = [
                (face_corners[0], 0usize),
                (face_corners[i], i),
                (face_corners[i + 1], i + 1),
            ];
            let mut output = [0u32; 3];
            for (slot_index, &(point_index, local_corner)) in triangle_corners.iter().enumerate() {
                let point_index = point_index.max(0) as usize;
                let Some(position) = points.get(point_index) else {
                    continue;
                };
                let corner_index = corner + local_corner;
                let normal = normals.as_ref().and_then(|normals| {
                    let index = primvar_index(
                        normal_indices.as_deref(),
                        normal_interpolation.as_deref(),
                        normals.len(),
                        corner_index,
                        point_index,
                        face,
                    )?;
                    normals
                        .get(index)
                        .map(|normal| (normal_matrix * *normal).normalize_or_zero())
                });
                let uv = uvs.as_ref().and_then(|uvs| {
                    let index = primvar_index(
                        uv_indices.as_deref(),
                        uv_interpolation.as_deref(),
                        uvs.len(),
                        corner_index,
                        point_index,
                        face,
                    )?;
                    uvs.get(index).copied()
                });
                output[slot_index] = builder.push(world.transform_point3(*position), normal, uv);
            }
            builder.triangle(output[0], output[1], output[2]);
        }
        corner += count;
    }

    let name = usd_prim_name(prim);
    builder.finish(name.clone(), name, material)
}

/// Direct `material:binding` on the mesh or its nearest ancestor. USD files
/// commonly bind at a parent scope, so walking up avoids a single material
/// swallowing every separated bake.
fn usd_material_name(stage: &openusd::usd::Stage, prim: &openusd::usd::Prim) -> Option<String> {
    let mut path = Some(prim.path().clone());
    while let Some(current) = path {
        if let Ok(property) = current.append_property("material:binding") {
            if let Ok(targets) = stage.relationship(property).targets() {
                if let Some(target) = targets.first() {
                    if let Some(name) = target.name() {
                        return Some(name.to_string());
                    }
                }
            }
        }
        path = current.parent();
    }
    None
}

fn find_uv_primvar(
    prim: &openusd::usd::Prim,
) -> (Option<Vec<Vec2>>, Option<String>, Option<Vec<i32>>) {
    const NAMES: &[&str] = &["primvars:st", "primvars:UVMap", "primvars:uv", "st"];
    for name in NAMES {
        let attribute = prim.attribute(*name);
        if let Some(uvs) = read_vec2_attr(&attribute) {
            let interpolation = interpolation(&attribute);
            let mut indices = primvar_indices(&attribute);
            if indices.is_none() {
                // Older exporters store primvar indices as an attribute
                // (`primvars:st.indices`) instead of metadata.
                let index_path = format!("{name}.indices");
                indices = read_int_attr(&prim.attribute(index_path.as_str()));
            }
            return (Some(uvs), interpolation, indices);
        }
    }
    (None, None, None)
}

fn read_vec2_attr(attribute: &openusd::usd::Attribute) -> Option<Vec<Vec2>> {
    use openusd::sdf::Value;
    match attribute.get::<Value>().ok().flatten()? {
        Value::Vec2fVec(values) => Some(values.into_iter().map(|v| Vec2::new(v.x, v.y)).collect()),
        Value::Vec2dVec(values) => Some(
            values
                .into_iter()
                .map(|v| Vec2::new(v.x as f32, v.y as f32))
                .collect(),
        ),
        Value::Vec2hVec(values) => Some(
            values
                .into_iter()
                .map(|v| Vec2::new(f32::from(v.x), f32::from(v.y)))
                .collect(),
        ),
        _ => None,
    }
}

fn read_vec3_attr(attribute: &openusd::usd::Attribute) -> Option<Vec<Vec3>> {
    use openusd::sdf::Value;
    match attribute.get::<Value>().ok().flatten()? {
        Value::Vec3fVec(values) => Some(
            values
                .into_iter()
                .map(|v| Vec3::new(v.x, v.y, v.z))
                .collect(),
        ),
        Value::Vec3dVec(values) => Some(
            values
                .into_iter()
                .map(|v| Vec3::new(v.x as f32, v.y as f32, v.z as f32))
                .collect(),
        ),
        Value::Vec3hVec(values) => Some(
            values
                .into_iter()
                .map(|v| Vec3::new(f32::from(v.x), f32::from(v.y), f32::from(v.z)))
                .collect(),
        ),
        _ => None,
    }
}

fn read_int_attr(attribute: &openusd::usd::Attribute) -> Option<Vec<i32>> {
    use openusd::sdf::Value;
    match attribute.get::<Value>().ok().flatten()? {
        Value::IntVec(values) => Some(values),
        Value::UintVec(values) => Some(values.into_iter().map(|value| value as i32).collect()),
        Value::Int64Vec(values) => Some(values.into_iter().map(|value| value as i32).collect()),
        _ => None,
    }
}

fn interpolation(attribute: &openusd::usd::Attribute) -> Option<String> {
    use openusd::sdf::Value;
    match attribute
        .get_metadata::<Value>("interpolation")
        .ok()
        .flatten()?
    {
        Value::Token(token) => Some(token.as_str().to_string()),
        Value::String(value) => Some(value),
        _ => None,
    }
}

fn primvar_indices(attribute: &openusd::usd::Attribute) -> Option<Vec<i32>> {
    use openusd::sdf::Value;
    match attribute.get_metadata::<Value>("indices").ok().flatten()? {
        Value::IntVec(values) => Some(values),
        Value::UintVec(values) => Some(values.into_iter().map(|value| value as i32).collect()),
        _ => None,
    }
}

fn primvar_index(
    indices: Option<&[i32]>,
    interpolation: Option<&str>,
    value_count: usize,
    corner: usize,
    point: usize,
    face: usize,
) -> Option<usize> {
    if value_count == 0 {
        return None;
    }
    let index = if let Some(indices) = indices {
        let source_index = match interpolation {
            Some("faceVarying") => corner,
            Some("uniform") => face,
            Some("constant") => 0,
            Some("vertex") | Some("varying") => point,
            _ if indices.len() > corner => corner,
            _ if indices.len() > point => point,
            _ => face,
        };
        let index = indices
            .get(source_index)
            .copied()
            .or_else(|| indices.first().copied())
            .unwrap_or(0);
        index.max(0) as usize
    } else {
        match interpolation.unwrap_or("vertex") {
            "faceVarying" => corner,
            "uniform" => face,
            "constant" => 0,
            "vertex" | "varying" => point,
            _ if value_count > corner => corner,
            _ if value_count > point => point,
            _ => 0,
        }
    };
    (index < value_count).then_some(index)
}

fn usd_prim_name(prim: &openusd::usd::Prim) -> String {
    prim.path()
        .as_str()
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("USD mesh")
        .to_string()
}

fn usd_matrix(matrix: openusd::gf::Matrix4d) -> Mat4 {
    let mut values = [0.0f32; 16];
    for (slot, value) in values.iter_mut().zip(matrix.0) {
        *slot = value as f32;
    }
    // `Matrix4d` is row-major; `from_cols_array` already transposes it into
    // the column-vector convention glam uses.
    Mat4::from_cols_array(&values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_file(extension: &str, contents: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "n-shade_mesh_io_{}_{}.{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default(),
            extension
        ));
        std::fs::write(&path, contents).expect("write temporary mesh");
        path
    }

    #[test]
    fn extensions_are_classified() {
        assert!(is_gltf_path(Path::new("hat.GLB")));
        assert!(is_gltf_path(Path::new("hat.gltf")));
        assert_eq!(mesh_format(Path::new("hat.obj")), Some(MeshFormat::Obj));
        assert_eq!(mesh_format(Path::new("hat.FBX")), Some(MeshFormat::Fbx));
        assert_eq!(mesh_format(Path::new("hat.dae")), Some(MeshFormat::Dae));
        assert_eq!(mesh_format(Path::new("hat.usda")), Some(MeshFormat::Usd));
        assert_eq!(mesh_format(Path::new("hat.usdc")), Some(MeshFormat::Usd));
        assert_eq!(mesh_format(Path::new("hat.usdz")), Some(MeshFormat::Usd));
        assert_eq!(mesh_format(Path::new("hat.png")), None);
        assert!(is_mesh_path(Path::new("hat.obj")));
        assert!(is_mesh_path(Path::new("hat.fbx")));
        assert!(!is_mesh_path(Path::new("hat.txt")));
    }

    #[test]
    fn mesh_only_gltf_reports_no_scene() {
        let scene_file = temp_file(
            "gltf",
            r#"{"asset":{"version":"2.0"},"scenes":[{"nodes":[]}],"scene":0}"#,
        );
        assert!(gltf_has_scene(&scene_file));
        let _ = std::fs::remove_file(scene_file);

        let mesh_only = temp_file("gltf", r#"{"asset":{"version":"2.0"}}"#);
        assert!(
            !gltf_has_scene(&mesh_only),
            "a GLB without scenes must take the native mesh fallback"
        );
        let _ = std::fs::remove_file(mesh_only);

        // Unparseable files stay on the Bevy path so its loader reports why.
        let broken = temp_file("gltf", "not json");
        assert!(gltf_has_scene(&broken));
        let _ = std::fs::remove_file(broken);
    }

    #[test]
    fn obj_is_read_with_uvs_and_normals() {
        let path = temp_file(
            "obj",
            "v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\n\
             vt 0 0\nvt 1 0\nvt 1 1\nvt 0 1\n\
             vn 0 0 1\n\
             f 1/1/1 2/2/1 3/3/1\nf 1/1/1 3/3/1 4/4/1\n",
        );
        let meshes = load_native(&path).expect("OBJ imports");
        let _ = std::fs::remove_file(path);
        assert_eq!(meshes.len(), 1);
        assert_eq!(meshes[0].triangle_count(), 2);
        assert_eq!(meshes[0].uvs.as_ref().map(Vec::len), Some(4));
        assert_eq!(meshes[0].normals.len(), meshes[0].positions.len());
    }

    #[test]
    fn dae_is_read_with_scene_transform() {
        let path = temp_file(
            "dae",
            r##"<?xml version="1.0" encoding="utf-8"?>
<COLLADA xmlns="http://www.collada.org/2005/11/COLLADASchema" version="1.4.1">
  <asset><created>1970-01-01T00:00:00Z</created><modified>1970-01-01T00:00:00Z</modified></asset>
  <library_geometries>
    <geometry id="Quad-mesh" name="Quad">
      <mesh>
        <source id="Quad-pos">
          <float_array id="Quad-pos-array" count="12">0 0 0 1 0 0 1 1 0 0 1 0</float_array>
          <technique_common>
            <accessor source="#Quad-pos-array" count="4" stride="3">
              <param name="X" type="float"/><param name="Y" type="float"/><param name="Z" type="float"/>
            </accessor>
          </technique_common>
        </source>
        <source id="Quad-uv">
          <float_array id="Quad-uv-array" count="8">0 0 1 0 1 1 0 1</float_array>
          <technique_common>
            <accessor source="#Quad-uv-array" count="4" stride="2">
              <param name="S" type="float"/><param name="T" type="float"/>
            </accessor>
          </technique_common>
        </source>
        <vertices id="Quad-vertices">
          <input semantic="POSITION" source="#Quad-pos"/>
        </vertices>
        <triangles count="2">
          <input semantic="VERTEX" source="#Quad-vertices" offset="0"/>
          <input semantic="TEXCOORD" source="#Quad-uv" offset="1" set="0"/>
          <p>0 0 1 1 2 2 0 0 2 2 3 3</p>
        </triangles>
      </mesh>
    </geometry>
  </library_geometries>
  <library_visual_scenes>
    <visual_scene id="Scene">
      <node id="Quad-node">
        <translate>1 2 3</translate>
        <instance_geometry url="#Quad-mesh"/>
      </node>
    </visual_scene>
  </library_visual_scenes>
  <scene><instance_visual_scene url="#Scene"/></scene>
</COLLADA>"##,
        );
        let meshes = load_native(&path).expect("DAE imports");
        let _ = std::fs::remove_file(path);
        assert_eq!(meshes.len(), 1);
        assert_eq!(meshes[0].triangle_count(), 2);
        assert_eq!(meshes[0].uvs.as_ref().map(Vec::len), Some(4));
        assert!(meshes[0]
            .positions
            .iter()
            .any(|position| (position[0] - 1.0).abs() < 1e-6 && (position[1] - 2.0).abs() < 1e-6));
    }

    #[test]
    fn usda_is_read_with_primvar_uvs() {
        let path = temp_file(
            "usda",
            r#"#usda 1.0
(
    defaultPrim = "Root"
)
def Xform "Root"
{
    double3 xformOp:translate = (1, 2, 3)
    uniform token[] xformOpOrder = ["xformOp:translate"]
    def Mesh "Quad"
    {
        int[] faceVertexCounts = [3, 3]
        int[] faceVertexIndices = [0, 1, 2, 0, 2, 3]
        point3f[] points = [(0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0)]
        texCoord2f[] primvars:st = [(0, 0), (1, 0), (1, 1), (0, 1)] (
            interpolation = "vertex"
        )
    }
}
"#,
        );
        let meshes = load_native(&path).expect("USDA imports");
        let _ = std::fs::remove_file(path);
        assert_eq!(meshes.len(), 1);
        assert_eq!(meshes[0].triangle_count(), 2);
        assert_eq!(meshes[0].uvs.as_ref().map(Vec::len), Some(4));
        assert!(meshes[0]
            .positions
            .iter()
            .any(|position| (position[0] - 2.0).abs() < 1e-5 && (position[1] - 3.0).abs() < 1e-5));
    }

    #[test]
    fn fbx_ascii_is_read() {
        let path = temp_file(
            "fbx",
            r#"; FBX 7.4.0 project file
FBXHeaderExtension:  {
    FBXHeaderVersion: 1003
    FBXVersion: 7400
}
GlobalSettings:  {
    Version: 1000
    Properties70:  {
        P: "UpAxis", "int", "Integer", "",1
        P: "UpAxisSign", "int", "Integer", "",1
        P: "FrontAxis", "int", "Integer", "",2
        P: "FrontAxisSign", "int", "Integer", "",1
        P: "CoordAxis", "int", "Integer", "",0
        P: "CoordAxisSign", "int", "Integer", "",1
        P: "UnitScaleFactor", "double", "Number", "",1
    }
}
Objects:  {
    Geometry: 1000, "Geometry::Quad", "Mesh" {
        Vertices: *12 {
            a: 0,0,0, 1,0,0, 1,1,0, 0,1,0
        }
        PolygonVertexIndex: *6 {
            a: 0,1,-3, 0,2,-4
        }
        LayerElementNormal: 0 {
            Version: 101
            Name: ""
            MappingInformationType: "ByPolygonVertex"
            ReferenceInformationType: "Direct"
            Normals: *18 {
                a: 0,0,1, 0,0,1, 0,0,1, 0,0,1, 0,0,1, 0,0,1
            }
        }
        LayerElementUV: 0 {
            Version: 101
            Name: "UVMap"
            MappingInformationType: "ByPolygonVertex"
            ReferenceInformationType: "IndexToDirect"
            UV: *8 {
                a: 0,0, 1,0, 1,1, 0,1
            }
            UVIndex: *6 {
                a: 0,1,2, 0,2,3
            }
        }
        LayerElementMaterial: 0 {
            Version: 101
            Name: ""
            MappingInformationType: "AllSame"
            ReferenceInformationType: "IndexToDirect"
            Materials: *1 { a: 0 }
        }
        Layer: 0 {
            Version: 100
            LayerElement:  {
                Type: "LayerElementNormal"
                TypedIndex: 0
            }
            LayerElement:  {
                Type: "LayerElementUV"
                TypedIndex: 0
            }
        }
    }
    Model: 2000, "Model::Quad", "Mesh" {
        Version: 232
        Properties70:  {
            P: "DefaultAttributeIndex", "int", "Integer", "",0
        }
    }
}
Connections:  {
    C: "OO",1000,2000
}
"#,
        );
        let meshes = load_native(&path).expect("FBX imports");
        let _ = std::fs::remove_file(path);
        assert_eq!(meshes.len(), 1);
        assert_eq!(meshes[0].triangle_count(), 2);
        assert_eq!(meshes[0].uvs.as_ref().map(Vec::len), Some(4));
        assert_eq!(meshes[0].normals.len(), meshes[0].positions.len());
    }
}
