//! Portable mesh payload used by the project archive.
//!
//! `.nsmesh` stores raw file-space geometry plus primitive groups. It is
//! independent of the source format (OBJ/FBX/DAE/USD/glTF) and has no external
//! references, so a project archive never needs the original mesh file to show
//! the preview, rebuild UV overlays, or run a lightmap bake.

use std::fmt::Write as _;

const MAGIC: &[u8; 8] = b"NSMESH01";
const VERSION: u16 = 1;
const HEADER_SIZE: usize = 32;

const FLAG_NORMALS: u16 = 1 << 0;
const FLAG_UVS: u16 = 1 << 1;
const FLAG_UV_BOTTOM_LEFT: u16 = 1 << 2;

/// One primitive range inside [`MeshAsset`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MeshGroup {
    pub name: String,
    pub object: String,
    pub material: Option<String>,
    /// First index in the shared index buffer.
    pub first_index: u32,
    /// Number of indices in this group (a multiple of three).
    pub index_count: u32,
}

/// Engine-agnostic mesh geometry stored in a project archive.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MeshAsset {
    pub name: String,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    /// `None` for meshes without UVs. When present, one entry per position.
    pub uvs: Option<Vec<[f32; 2]>>,
    pub indices: Vec<u32>,
    pub groups: Vec<MeshGroup>,
    /// UV origin convention used by the source format.
    pub uv_bottom_left: bool,
}

impl MeshAsset {
    pub fn vertex_count(&self) -> usize {
        self.positions.len()
    }

    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// Group index per triangle. Missing groups read as 0.
    pub fn triangle_groups(&self) -> Vec<u32> {
        let triangles = self.triangle_count();
        let mut groups = vec![0u32; triangles];
        for (index, group) in self.groups.iter().enumerate() {
            let start = (group.first_index as usize / 3).min(triangles);
            let end =
                ((group.first_index as usize + group.index_count as usize) / 3).min(triangles);
            for slot in groups.iter_mut().take(end).skip(start) {
                *slot = index as u32;
            }
        }
        groups
    }

    /// Validate indices, attribute counts, and groups before encoding.
    pub fn validate(&self) -> Result<(), String> {
        if self.positions.is_empty() {
            return Err("mesh has no positions".into());
        }
        if self.positions.len() > u32::MAX as usize {
            return Err("mesh has too many vertices".into());
        }
        if !self.normals.is_empty() && self.normals.len() != self.positions.len() {
            return Err("normal count does not match position count".into());
        }
        if let Some(uvs) = &self.uvs {
            if uvs.len() != self.positions.len() {
                return Err("UV count does not match position count".into());
            }
        }
        if !self.indices.len().is_multiple_of(3) {
            return Err("index count is not a multiple of three".into());
        }
        if self.indices.len() > u32::MAX as usize {
            return Err("mesh has too many indices".into());
        }
        for &index in &self.indices {
            if index as usize >= self.positions.len() {
                return Err(format!("index {index} is out of range"));
            }
        }
        for (group_index, group) in self.groups.iter().enumerate() {
            let end = group.first_index as u64 + group.index_count as u64;
            if end > self.indices.len() as u64 {
                return Err(format!("group {group_index} extends past the index buffer"));
            }
            if group.index_count % 3 != 0 {
                return Err(format!("group {group_index} is not triangle-aligned"));
            }
        }
        for (index, value) in self.positions.iter().enumerate() {
            if value.iter().any(|v| !v.is_finite()) {
                return Err(format!("position {index} is not finite"));
            }
        }
        Ok(())
    }

    /// Encode the portable `.nsmesh` binary payload.
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        self.validate()?;

        let mut strings = Vec::new();
        let mut group_offsets = Vec::with_capacity(self.groups.len());
        for group in &self.groups {
            let name = push_string(&mut strings, &group.name);
            let object = push_string(&mut strings, &group.object);
            let material = group
                .material
                .as_deref()
                .map(|value| push_string(&mut strings, value))
                .unwrap_or(u32::MAX);
            group_offsets.push((name, object, material));
        }

        let mut flags = 0u16;
        let has_normals = self.normals.len() == self.positions.len() && !self.normals.is_empty();
        let has_uvs = self.uvs.is_some();
        if has_normals {
            flags |= FLAG_NORMALS;
        }
        if has_uvs {
            flags |= FLAG_UVS;
        }
        if self.uv_bottom_left {
            flags |= FLAG_UV_BOTTOM_LEFT;
        }

        let vertex_count = self.positions.len() as u32;
        let index_count = self.indices.len() as u32;
        let group_count = self.groups.len() as u32;
        let string_bytes = strings.len() as u32;

        let mut out = Vec::with_capacity(
            HEADER_SIZE
                + self.positions.len() * 12
                + self.normals.len() * 12
                + self.uvs.as_ref().map_or(0, |uvs| uvs.len() * 8)
                + self.indices.len() * 4
                + self.groups.len() * 20
                + strings.len(),
        );
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&vertex_count.to_le_bytes());
        out.extend_from_slice(&index_count.to_le_bytes());
        out.extend_from_slice(&group_count.to_le_bytes());
        out.extend_from_slice(&string_bytes.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());

        for position in &self.positions {
            for value in position {
                out.extend_from_slice(&value.to_le_bytes());
            }
        }
        if has_normals {
            for normal in &self.normals {
                for value in normal {
                    out.extend_from_slice(&value.to_le_bytes());
                }
            }
        }
        if let Some(uvs) = &self.uvs {
            for uv in uvs {
                for value in uv {
                    out.extend_from_slice(&value.to_le_bytes());
                }
            }
        }
        for index in &self.indices {
            out.extend_from_slice(&index.to_le_bytes());
        }
        for (group, (name, object, material)) in self.groups.iter().zip(group_offsets) {
            out.extend_from_slice(&group.first_index.to_le_bytes());
            out.extend_from_slice(&group.index_count.to_le_bytes());
            out.extend_from_slice(&name.to_le_bytes());
            out.extend_from_slice(&object.to_le_bytes());
            out.extend_from_slice(&material.to_le_bytes());
        }
        out.extend_from_slice(&strings);
        Ok(out)
    }

    /// Decode a `.nsmesh` payload.
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() < HEADER_SIZE {
            return Err("mesh payload is shorter than its header".into());
        }
        if bytes[..8] != MAGIC[..] {
            return Err("mesh payload has an unknown magic".into());
        }
        let version = read_u16(bytes, 8)?;
        if version != VERSION {
            return Err(format!("unsupported mesh payload version {version}"));
        }
        let flags = read_u16(bytes, 10)?;
        let vertex_count = read_u32(bytes, 12)? as usize;
        let index_count = read_u32(bytes, 16)? as usize;
        let group_count = read_u32(bytes, 20)? as usize;
        let string_bytes = read_u32(bytes, 24)? as usize;

        let has_normals = flags & FLAG_NORMALS != 0;
        let has_uvs = flags & FLAG_UVS != 0;
        let uv_bottom_left = flags & FLAG_UV_BOTTOM_LEFT != 0;

        let position_bytes = vertex_count
            .checked_mul(12)
            .ok_or_else(|| "mesh position size overflow".to_string())?;
        let normal_bytes = if has_normals {
            vertex_count
                .checked_mul(12)
                .ok_or_else(|| "mesh normal size overflow".to_string())?
        } else {
            0
        };
        let uv_bytes = if has_uvs {
            vertex_count
                .checked_mul(8)
                .ok_or_else(|| "mesh UV size overflow".to_string())?
        } else {
            0
        };
        let index_bytes = index_count
            .checked_mul(4)
            .ok_or_else(|| "mesh index size overflow".to_string())?;
        let group_bytes = group_count
            .checked_mul(20)
            .ok_or_else(|| "mesh group size overflow".to_string())?;
        let expected = HEADER_SIZE
            .checked_add(position_bytes)
            .and_then(|value| value.checked_add(normal_bytes))
            .and_then(|value| value.checked_add(uv_bytes))
            .and_then(|value| value.checked_add(index_bytes))
            .and_then(|value| value.checked_add(group_bytes))
            .and_then(|value| value.checked_add(string_bytes))
            .ok_or_else(|| "mesh payload size overflow".to_string())?;
        if bytes.len() != expected {
            return Err(format!(
                "mesh payload has {} bytes, expected {expected}",
                bytes.len()
            ));
        }

        let mut offset = HEADER_SIZE;
        let mut positions = Vec::with_capacity(vertex_count);
        for _ in 0..vertex_count {
            positions.push([
                read_f32(bytes, offset)?,
                read_f32(bytes, offset + 4)?,
                read_f32(bytes, offset + 8)?,
            ]);
            offset += 12;
        }
        let mut normals = Vec::new();
        if has_normals {
            normals.reserve(vertex_count);
            for _ in 0..vertex_count {
                normals.push([
                    read_f32(bytes, offset)?,
                    read_f32(bytes, offset + 4)?,
                    read_f32(bytes, offset + 8)?,
                ]);
                offset += 12;
            }
        }
        let uvs = if has_uvs {
            let mut uvs = Vec::with_capacity(vertex_count);
            for _ in 0..vertex_count {
                uvs.push([read_f32(bytes, offset)?, read_f32(bytes, offset + 4)?]);
                offset += 8;
            }
            Some(uvs)
        } else {
            None
        };
        let mut indices = Vec::with_capacity(index_count);
        for _ in 0..index_count {
            indices.push(read_u32(bytes, offset)?);
            offset += 4;
        }

        let group_start = offset;
        let string_start = group_start + group_bytes;
        let strings = &bytes[string_start..string_start + string_bytes];
        let mut groups = Vec::with_capacity(group_count);
        for index in 0..group_count {
            let base = group_start + index * 20;
            let first_index = read_u32(bytes, base)?;
            let index_count = read_u32(bytes, base + 4)?;
            let name = read_string(strings, read_u32(bytes, base + 8)?)?;
            let object = read_string(strings, read_u32(bytes, base + 12)?)?;
            let material_offset = read_u32(bytes, base + 16)?;
            let material = if material_offset == u32::MAX {
                None
            } else {
                Some(read_string(strings, material_offset)?)
            };
            groups.push(MeshGroup {
                name,
                object,
                material,
                first_index,
                index_count,
            });
        }

        let mesh = Self {
            name: String::new(),
            positions,
            normals,
            uvs,
            indices,
            groups,
            uv_bottom_left,
        };
        mesh.validate()?;
        Ok(mesh)
    }
}

fn push_string(strings: &mut Vec<u8>, value: &str) -> u32 {
    let offset = strings.len() as u32;
    let bytes = value.as_bytes();
    let length = bytes.len().min(u32::MAX as usize) as u32;
    strings.extend_from_slice(&length.to_le_bytes());
    strings.extend_from_slice(&bytes[..length as usize]);
    offset
}

fn read_string(strings: &[u8], offset: u32) -> Result<String, String> {
    let offset = offset as usize;
    if offset + 4 > strings.len() {
        return Err("mesh string offset is out of range".into());
    }
    let length = u32::from_le_bytes(
        strings[offset..offset + 4]
            .try_into()
            .map_err(|_| "invalid mesh string length".to_string())?,
    ) as usize;
    let start = offset + 4;
    let end = start
        .checked_add(length)
        .ok_or_else(|| "mesh string length overflow".to_string())?;
    if end > strings.len() {
        return Err("mesh string extends past the string table".into());
    }
    String::from_utf8(strings[start..end].to_vec())
        .map_err(|error| format!("mesh string is not UTF-8: {error}"))
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, String> {
    let slice = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| "truncated mesh payload".to_string())?;
    Ok(u16::from_le_bytes(
        slice.try_into().map_err(|_| "invalid u16".to_string())?,
    ))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, String> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| "truncated mesh payload".to_string())?;
    Ok(u32::from_le_bytes(
        slice.try_into().map_err(|_| "invalid u32".to_string())?,
    ))
}

fn read_f32(bytes: &[u8], offset: usize) -> Result<f32, String> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| "truncated mesh payload".to_string())?;
    Ok(f32::from_le_bytes(
        slice.try_into().map_err(|_| "invalid f32".to_string())?,
    ))
}

/// Human-readable mesh summary used by diagnostics and status lines.
pub fn mesh_summary(mesh: &MeshAsset) -> String {
    let mut summary = String::new();
    let _ = write!(
        summary,
        "{} vertices, {} triangles, {} groups",
        mesh.vertex_count(),
        mesh.triangle_count(),
        mesh.groups.len()
    );
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mesh_asset_round_trips() {
        let mesh = MeshAsset {
            name: "panel".into(),
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            normals: vec![[0.0, 0.0, 1.0]; 3],
            uvs: Some(vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]),
            indices: vec![0, 1, 2],
            groups: vec![MeshGroup {
                name: "face".into(),
                object: "panel".into(),
                material: Some("metal".into()),
                first_index: 0,
                index_count: 3,
            }],
            uv_bottom_left: true,
        };
        let bytes = mesh.encode().unwrap();
        let decoded = MeshAsset::decode(&bytes).unwrap();
        assert_eq!(decoded.positions, mesh.positions);
        assert_eq!(decoded.uvs, mesh.uvs);
        assert_eq!(decoded.indices, mesh.indices);
        assert!(decoded.uv_bottom_left);
        assert_eq!(decoded.groups[0].material.as_deref(), Some("metal"));
    }
}
