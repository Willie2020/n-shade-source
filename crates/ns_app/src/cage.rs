//! Bake cage overlay for the 3D preview.
//!
//! A high-to-low bake starts its rays on a cage: the low-poly hull pushed out
//! along the vertex normals by the cage percentage. The baker casts both ways
//! from that hull, so the authored distance is worth seeing. This module draws
//! the hull in the preview, in the teal wireframe style of a Toolbag cage.
//!
//! Topology is built once per imported mesh; moving the cage slider only
//! rewrites the position attribute of the hull and wire meshes, so dragging the
//! value stays cheap.

use std::collections::HashSet;

use bevy::asset::RenderAssetUsages;
use bevy::prelude::*;
use bevy::render::mesh::{Indices, Mesh, PrimitiveTopology};
use glam::Vec3;
use ns_bake::BakeMode;

use crate::bake::BakeRuntime;
use crate::import::MeshImport;

/// Teal hull and wire tint. Toolbag draws its cage in this green-cyan family so
/// it never competes with the orange selection colour of the editor.
const CAGE_TINT: Color = Color::srgb(0.36, 1.0, 0.83);

pub struct BakeCagePlugin;

impl Plugin for BakeCagePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CageState>()
            .add_systems(Update, sync_cage);
    }
}

/// Which mesh of the cage a spawned entity carries.
#[derive(Component, Clone, Copy, PartialEq, Eq)]
enum CagePart {
    /// Translucent offset surface.
    Hull,
    /// Line-list wireframe on the same vertices.
    Wire,
}

#[derive(Resource, Default)]
struct CageState {
    hull_material: Option<Handle<StandardMaterial>>,
    wire_material: Option<Handle<StandardMaterial>>,
    hull_mesh: Option<Handle<Mesh>>,
    wire_mesh: Option<Handle<Mesh>>,
    built_scene: Option<Entity>,
    built_triangles: usize,
    built_pct: f32,
    base: Option<CageBase>,
}

impl CageState {
    fn clear_build(&mut self) {
        self.built_scene = None;
        self.built_triangles = 0;
        self.built_pct = 0.0;
        self.base = None;
    }

    /// Drops the hull and wire mesh assets. The entities that used them are
    /// despawned by the caller; without this the `Assets<Mesh>` store would keep
    /// every rebuild alive for the rest of the session.
    fn release_meshes(&mut self, meshes: &mut Assets<Mesh>) {
        if let Some(handle) = self.hull_mesh.take() {
            meshes.remove(&handle);
        }
        if let Some(handle) = self.wire_mesh.take() {
            meshes.remove(&handle);
        }
        self.clear_build();
    }
}

/// Un-offset vertices of the low-poly plus the derived edge list. Positions are
/// rewritten in place when the cage value changes.
struct CageBase {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    surface_indices: Vec<u32>,
    wire_indices: Vec<u32>,
    /// Longest side of the low-poly bounds; the cage percentage scales this.
    extent: f32,
}

#[allow(clippy::too_many_arguments)]
fn sync_cage(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut state: ResMut<CageState>,
    import: Res<MeshImport>,
    bake: Res<BakeRuntime>,
    parts: Query<(Entity, &CagePart, &Mesh3d)>,
    mesh_entities: Query<&Mesh3d>,
    children: Query<&Children>,
) {
    let scene = import.scene.filter(|_| import.ready);
    let wanted = bake.show_cage && bake.settings.mode == BakeMode::HighToLow && scene.is_some();

    if !wanted {
        if !parts.is_empty() {
            for (entity, _, _) in &parts {
                commands.entity(entity).despawn();
            }
        }
        state.release_meshes(&mut meshes);
        return;
    }
    let Some(scene) = scene else {
        return;
    };
    let pct = bake.settings.cage_offset_pct.max(0.0);

    let rebuild = state.built_scene != Some(scene)
        || state.built_triangles != import.triangles
        || state.base.is_none();

    if rebuild {
        for (entity, _, _) in &parts {
            commands.entity(entity).despawn();
        }
        state.release_meshes(&mut meshes);
        let handles = collect_meshes(&children, &mesh_entities, scene);
        let Some(base) = build_base(&meshes, &handles) else {
            return;
        };
        let offset = pct * 0.01 * base.extent;
        let (hull_material, wire_material) = ensure_materials(&mut state, &mut materials);
        let hull = meshes.add(hull_mesh(&base, offset));
        let wire = meshes.add(wire_mesh(&base, offset));
        commands.entity(scene).with_children(|parent| {
            parent.spawn((
                Mesh3d(hull.clone()),
                MeshMaterial3d(hull_material),
                CagePart::Hull,
                Name::new("Bake cage hull"),
            ));
            parent.spawn((
                Mesh3d(wire.clone()),
                MeshMaterial3d(wire_material),
                CagePart::Wire,
                Name::new("Bake cage wireframe"),
            ));
        });
        state.built_scene = Some(scene);
        state.built_triangles = import.triangles;
        state.built_pct = pct;
        state.base = Some(base);
        state.hull_mesh = Some(hull);
        state.wire_mesh = Some(wire);
        return;
    }

    if (state.built_pct - pct).abs() > 0.001 {
        let positions = {
            let base = state.base.as_ref().expect("cage base");
            displaced_positions(base, pct * 0.01 * base.extent)
        };
        for (_, _, mesh_handle) in &parts {
            if let Some(mut mesh) = meshes.get_mut(&mesh_handle.0) {
                mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions.clone());
            }
        }
        state.built_pct = pct;
    }
}

fn collect_meshes(
    children: &Query<&Children>,
    mesh_entities: &Query<&Mesh3d>,
    scene: Entity,
) -> Vec<Handle<Mesh>> {
    let mut seen = HashSet::new();
    let mut handles = Vec::new();
    for entity in children.iter_descendants(scene) {
        if let Ok(mesh) = mesh_entities.get(entity) {
            if seen.insert(mesh.0.id()) {
                handles.push(mesh.0.clone());
            }
        }
    }
    handles
}

fn build_base(meshes: &Assets<Mesh>, handles: &[Handle<Mesh>]) -> Option<CageBase> {
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut surface_indices = Vec::new();
    let mut wire_indices = Vec::new();
    let mut min = Vec3::splat(f32::MAX);
    let mut max = Vec3::splat(f32::MIN);
    let mut used = 0usize;

    for handle in handles {
        let Some(mesh) = meshes.get(handle) else {
            continue;
        };
        let Some(source_positions) = mesh
            .attribute(Mesh::ATTRIBUTE_POSITION)
            .and_then(|attribute| attribute.as_float3())
        else {
            continue;
        };
        let Some(indices) = mesh.indices() else {
            continue;
        };
        let indices: Vec<u32> = match indices {
            Indices::U16(values) => values.iter().map(|index| *index as u32).collect(),
            Indices::U32(values) => values.clone(),
        };
        if source_positions.is_empty() || indices.is_empty() {
            continue;
        }
        for position in source_positions {
            let point = Vec3::from_array(*position);
            min = min.min(point);
            max = max.max(point);
        }
        let supplied = mesh
            .attribute(Mesh::ATTRIBUTE_NORMAL)
            .and_then(|attribute| attribute.as_float3())
            .filter(|values| values.len() == source_positions.len())
            .map(|values| values.to_vec());
        let part_normals = supplied.unwrap_or_else(|| vertex_normals(source_positions, &indices));

        let base = positions.len() as u32;
        positions.extend_from_slice(source_positions);
        normals.extend_from_slice(&part_normals);

        for index in &indices {
            surface_indices.push(base + *index);
        }
        let mut seen = HashSet::new();
        for triangle in indices.chunks_exact(3) {
            for (a, b) in [
                (triangle[0], triangle[1]),
                (triangle[1], triangle[2]),
                (triangle[2], triangle[0]),
            ] {
                let key = if a < b { (a, b) } else { (b, a) };
                if seen.insert(key) {
                    wire_indices.push(base + a);
                    wire_indices.push(base + b);
                }
            }
        }
        used += 1;
    }

    if used == 0 {
        return None;
    }
    let extent = (max - min).max_element().max(1e-5);
    Some(CageBase {
        positions,
        normals,
        surface_indices,
        wire_indices,
        extent,
    })
}

/// Area-weighted vertex normals for meshes that ship without them, so the cage
/// can still be built from a minimal OBJ.
fn vertex_normals(positions: &[[f32; 3]], indices: &[u32]) -> Vec<[f32; 3]> {
    let mut normals = vec![Vec3::ZERO; positions.len()];
    for triangle in indices.chunks_exact(3) {
        let (a, b, c) = (
            triangle[0] as usize,
            triangle[1] as usize,
            triangle[2] as usize,
        );
        if a >= positions.len() || b >= positions.len() || c >= positions.len() {
            continue;
        }
        let pa = Vec3::from_array(positions[a]);
        let pb = Vec3::from_array(positions[b]);
        let pc = Vec3::from_array(positions[c]);
        // Un-normalized cross product weights by triangle area.
        let face = (pb - pa).cross(pc - pa);
        normals[a] += face;
        normals[b] += face;
        normals[c] += face;
    }
    normals
        .into_iter()
        .map(|normal| normal.normalize_or_zero().to_array())
        .collect()
}

fn displaced_positions(base: &CageBase, offset: f32) -> Vec<[f32; 3]> {
    base.positions
        .iter()
        .zip(base.normals.iter())
        .map(|(position, normal)| {
            (Vec3::from_array(*position) + Vec3::from_array(*normal) * offset).to_array()
        })
        .collect()
}

fn hull_mesh(base: &CageBase, offset: f32) -> Mesh {
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, displaced_positions(base, offset));
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, base.normals.clone());
    mesh.insert_indices(Indices::U32(base.surface_indices.clone()));
    mesh
}

fn wire_mesh(base: &CageBase, offset: f32) -> Mesh {
    let mut mesh = Mesh::new(PrimitiveTopology::LineList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, displaced_positions(base, offset));
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, base.normals.clone());
    mesh.insert_indices(Indices::U32(base.wire_indices.clone()));
    mesh
}

fn ensure_materials(
    state: &mut CageState,
    materials: &mut Assets<StandardMaterial>,
) -> (Handle<StandardMaterial>, Handle<StandardMaterial>) {
    let hull = state
        .hull_material
        .get_or_insert_with(|| {
            materials.add(StandardMaterial {
                base_color: CAGE_TINT.with_alpha(0.09),
                alpha_mode: AlphaMode::Blend,
                unlit: true,
                cull_mode: None,
                double_sided: true,
                ..default()
            })
        })
        .clone();
    let wire = state
        .wire_material
        .get_or_insert_with(|| {
            materials.add(StandardMaterial {
                base_color: CAGE_TINT.with_alpha(0.9),
                alpha_mode: AlphaMode::Blend,
                unlit: true,
                cull_mode: None,
                double_sided: true,
                ..default()
            })
        })
        .clone();
    (hull, wire)
}
