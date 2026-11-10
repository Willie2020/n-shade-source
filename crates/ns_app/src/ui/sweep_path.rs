//! Sweep-path handles drawn directly on the 3D model.
//!
//! A sweep operation places copies of a shape along a path guide's spine. While
//! the sweep is selected, this module shows that guide's anchors and Bezier
//! handles on the imported mesh. A drag raycasts the same normalized triangles
//! the preview renders, converts the hit's barycentric UV back into document
//! pixels, and edits exactly that one node. The swept source shapes are never
//! scaled: only the guide changes, so copies keep their size and follow the new
//! spine.
//!
//! The 2D canvas reads the same path nodes, so every surface edit is reflected
//! there immediately. One pointer gesture is one undo step.

use super::*;
use glam::{Quat, Vec3};

use crate::import::SurfaceTriangle;
use crate::operations::OperationKind;

/// Screen radius for grabbing an anchor or a Bezier handle.
const HIT_RADIUS: f32 = 9.0;
/// Corner markers are deliberately small so they do not hide the model detail.
const ANCHOR_SIZE: f32 = 7.0;
const HANDLE_SIZE: f32 = 6.0;

pub(crate) struct SweepPathInput {
    /// A path handle is being dragged; orbit and layer gizmos must stand down.
    pub dragging: bool,
    pub hovering: Option<SweepPathHover>,
}

impl Default for SweepPathInput {
    fn default() -> Self {
        Self {
            dragging: false,
            hovering: None,
        }
    }
}

#[derive(Clone, Copy)]
struct SurfaceHit {
    doc: Vec2,
    uv: Vec2,
}

/// Mapping between the 2D document and the normalized preview triangles.
struct SurfaceMap<'a> {
    triangles: &'a [SurfaceTriangle],
    rotation: Quat,
    doc_size: Vec2,
    uv_bottom_left: bool,
}

impl<'a> SurfaceMap<'a> {
    fn new(mesh_import: &'a MeshImport, doc_size: Vec2) -> Self {
        Self {
            triangles: &mesh_import.surface,
            rotation: mesh_import.surface_rotation,
            doc_size,
            uv_bottom_left: mesh_import.uv_bottom_left,
        }
    }

    fn doc_to_uv(&self, point: Vec2) -> Vec2 {
        let u = point.x / self.doc_size.x.max(1.0);
        let v = point.y / self.doc_size.y.max(1.0);
        // OBJ/FBX/DAE/USD use a bottom-left UV origin; glTF is top-left.
        Vec2::new(u, if self.uv_bottom_left { 1.0 - v } else { v })
    }

    fn uv_to_doc(&self, uv: Vec2) -> Vec2 {
        let v = if self.uv_bottom_left {
            1.0 - uv.y
        } else {
            uv.y
        };
        Vec2::new(uv.x * self.doc_size.x, v * self.doc_size.y)
    }

    /// First triangle with this UV, used to place a path node on the surface.
    fn point_for_uv(&self, uv: Vec2) -> Option<Vec3> {
        for triangle in self.triangles {
            let Some(weights) = barycentric(triangle.uvs, uv) else {
                continue;
            };
            return Some(
                Vec3::from_array(triangle.positions[0]) * weights.x
                    + Vec3::from_array(triangle.positions[1]) * weights.y
                    + Vec3::from_array(triangle.positions[2]) * weights.z,
            );
        }
        None
    }

    fn project(&self, camera: &GizmoCamera, doc: Vec2) -> Option<egui::Pos2> {
        let point = self.point_for_uv(self.doc_to_uv(doc))?;
        camera.project(self.rotation * point)
    }

    /// Nearest surface hit under a ray. The ray is rotated into mesh space so
    /// the hover angle of an auto-rotating preview stays exact.
    fn raycast(&self, origin: Vec3, direction: Vec3) -> Option<SurfaceHit> {
        let inverse = self.rotation.inverse();
        let origin = inverse * origin;
        let direction = inverse * direction;
        let mut best: Option<(f32, &SurfaceTriangle, Vec3)> = None;
        for triangle in self.triangles {
            let a = Vec3::from_array(triangle.positions[0]);
            let b = Vec3::from_array(triangle.positions[1]);
            let c = Vec3::from_array(triangle.positions[2]);
            let Some((distance, weights)) = ray_triangle(origin, direction, a, b, c) else {
                continue;
            };
            if best.is_none_or(|(nearest, _, _)| distance < nearest) {
                best = Some((distance, triangle, weights));
            }
        }
        let (_, triangle, weights) = best?;
        let uv = Vec2::from_array(triangle.uvs[0]) * weights.x
            + Vec2::from_array(triangle.uvs[1]) * weights.y
            + Vec2::from_array(triangle.uvs[2]) * weights.z;
        Some(SurfaceHit {
            doc: self.uv_to_doc(uv),
            uv,
        })
    }
}

fn barycentric(uvs: [[f32; 2]; 3], point: Vec2) -> Option<Vec3> {
    let a = Vec2::from_array(uvs[0]);
    let b = Vec2::from_array(uvs[1]);
    let c = Vec2::from_array(uvs[2]);
    let v0 = b - a;
    let v1 = c - a;
    let v2 = point - a;
    let denominator = v0.x * v1.y - v1.x * v0.y;
    if denominator.abs() < 1e-9 {
        return None;
    }
    let y = (v2.x * v1.y - v1.x * v2.y) / denominator;
    let z = (v0.x * v2.y - v2.x * v0.y) / denominator;
    let x = 1.0 - y - z;
    (x >= -1e-4 && y >= -1e-4 && z >= -1e-4).then_some(Vec3::new(x, y, z))
}

/// Moller-Trumbore ray/triangle intersection. Returns the ray distance and the
/// barycentric weights for the triangle's three vertices.
fn ray_triangle(origin: Vec3, direction: Vec3, a: Vec3, b: Vec3, c: Vec3) -> Option<(f32, Vec3)> {
    let edge1 = b - a;
    let edge2 = c - a;
    let pvec = direction.cross(edge2);
    let determinant = edge1.dot(pvec);
    if determinant.abs() < 1e-9 {
        return None;
    }
    let inverse = 1.0 / determinant;
    let tvec = origin - a;
    let u = tvec.dot(pvec) * inverse;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let qvec = tvec.cross(edge1);
    let v = direction.dot(qvec) * inverse;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let distance = edge2.dot(qvec) * inverse;
    (distance > 1e-5).then_some((distance, Vec3::new(1.0 - u - v, u, v)))
}

/// Path guide whose handles belong on the model right now.
///
/// The primary selection may be the swept source (the usual case) or the guide
/// itself. In both cases only an enabled Sweep entry makes this editor active.
pub(crate) fn sweep_path_target(state: &EditorState, doc: &ns_core::Document) -> Option<LayerId> {
    let selected = doc.layer_by_path(state.selected.as_deref()?)?;
    let sweeps = |owner: LayerId| {
        state.operation_stacks.get(&owner).is_some_and(|stack| {
            stack
                .iter()
                .any(|entry| entry.enabled && entry.kind == OperationKind::Sweep)
        })
    };
    if sweeps(selected.id) {
        let stack = state.operation_stacks.get(&selected.id)?;
        let guide = stack
            .iter()
            .rev()
            .find(|entry| entry.enabled && entry.kind == OperationKind::Sweep)?
            .sweep
            .path?;
        if doc
            .find_layer(guide)
            .is_some_and(|layer| layer.is_path_guide())
        {
            return Some(guide);
        }
    }
    // Selecting the guide itself is the natural way to tweak its handles.
    if selected.is_path_guide()
        && state.operation_stacks.values().any(|stack| {
            stack.iter().any(|entry| {
                entry.enabled
                    && entry.kind == OperationKind::Sweep
                    && entry.sweep.path == Some(selected.id)
            })
        })
    {
        return Some(selected.id);
    }
    None
}

fn guide_path(doc: &ns_core::Document, id: LayerId) -> Option<&ns_core::Path> {
    match &doc.find_layer(id)?.kind {
        LayerKind::Shape(shape) => match &shape.source {
            ShapeSource::Path(path) => Some(path),
            _ => None,
        },
        _ => None,
    }
}

/// Move or adjust one handle of a guide path. Returns true when the document
/// changed. Only the path guide is touched, so arrayed sweep copies keep their
/// original size and simply march along the new spine.
pub(crate) fn apply_sweep_handle(
    doc: &mut DocumentRes,
    guide_id: LayerId,
    hover: SweepPathHover,
    doc_position: Vec2,
    break_handles: bool,
) -> bool {
    let Some(layer) = doc.0.find_layer_mut(guide_id) else {
        return false;
    };
    if layer.locked {
        return false;
    }
    let LayerKind::Shape(shape) = &mut layer.kind else {
        return false;
    };
    let ShapeSource::Path(path) = &mut shape.source else {
        return false;
    };
    let Some(node) = path
        .subpaths
        .get_mut(hover.node.subpath)
        .and_then(|subpath| subpath.nodes.get_mut(hover.node.node))
    else {
        return false;
    };
    match hover.kind {
        NodeDragKind::Move => node.pos = doc_position,
        NodeDragKind::HandleIn | NodeDragKind::HandleOut => {
            let handle = doc_position - node.pos;
            if handle.length_squared() < 0.25 {
                return false;
            }
            set_handle(node, hover.kind, handle, break_handles);
        }
        NodeDragKind::PullHandles => {
            let handle = doc_position - node.pos;
            if handle.length_squared() < 0.25 {
                return false;
            }
            node.kind = NodeKind::Symmetric;
            node.handle_out = handle;
            node.handle_in = -handle;
        }
    }
    layer.touch();
    true
}

fn handle_positions(
    path: &ns_core::Path,
    map: &SurfaceMap<'_>,
    camera: &GizmoCamera,
) -> Vec<(SweepPathHover, egui::Pos2)> {
    let mut handles = Vec::new();
    for (subpath_index, subpath) in path.subpaths.iter().enumerate() {
        for (node_index, node) in subpath.nodes.iter().enumerate() {
            let node_ref = NodeRef {
                subpath: subpath_index,
                node: node_index,
            };
            if let Some(screen) = map.project(camera, node.pos) {
                handles.push((
                    SweepPathHover {
                        node: node_ref,
                        kind: NodeDragKind::Move,
                    },
                    screen,
                ));
            }
            for (kind, offset) in [
                (NodeDragKind::HandleIn, node.handle_in),
                (NodeDragKind::HandleOut, node.handle_out),
            ] {
                if offset.length_squared() < 1e-4 {
                    continue;
                }
                if let Some(screen) = map.project(camera, node.pos + offset) {
                    handles.push((
                        SweepPathHover {
                            node: node_ref,
                            kind,
                        },
                        screen,
                    ));
                }
            }
        }
    }
    handles
}

fn hit_test(
    handles: &[(SweepPathHover, egui::Pos2)],
    pointer: egui::Pos2,
) -> Option<SweepPathHover> {
    let mut best: Option<(f32, SweepPathHover)> = None;
    for (handle, screen) in handles {
        let distance = screen.distance(pointer);
        if distance > HIT_RADIUS {
            continue;
        }
        // Anchors own the hit when an anchor and a zero-length handle overlap.
        let priority = if handle.kind == NodeDragKind::Move {
            distance - 1.0
        } else {
            distance
        };
        if best.is_none_or(|(nearest, _)| priority < nearest) {
            best = Some((priority, *handle));
        }
    }
    best.map(|(_, handle)| handle)
}

/// Hover, drag and undo handling for the on-model sweep path.
pub(crate) fn sweep_path_input(
    ui: &egui::Ui,
    response: &egui::Response,
    rect: egui::Rect,
    state: &mut EditorState,
    doc: &mut DocumentRes,
    mesh_import: &MeshImport,
) -> SweepPathInput {
    let Some(guide_id) = sweep_path_target(state, &doc.0) else {
        state.sweep_path_drag = None;
        state.sweep_path_hover = None;
        state.sweep_path_readout.clear();
        return SweepPathInput::default();
    };
    let Some(path) = guide_path(&doc.0, guide_id) else {
        state.sweep_path_drag = None;
        return SweepPathInput::default();
    };
    if mesh_import.surface.is_empty() {
        state.sweep_path_readout = "Import a UV-mapped mesh to edit the sweep path on it".into();
        return SweepPathInput::default();
    }

    let map = SurfaceMap::new(
        mesh_import,
        Vec2::new(doc.0.size.x as f32, doc.0.size.y as f32),
    );
    let camera = camera_for(rect, state);
    let handles = handle_positions(path, &map, &camera);
    let hovered = response
        .hover_pos()
        .and_then(|pointer| hit_test(&handles, pointer));
    state.sweep_path_hover = hovered;

    if response.drag_started_by(egui::PointerButton::Primary) {
        if let Some(handle) = hovered {
            if doc.0.is_locked(guide_id) {
                state.status = "The sweep path is locked · unlock its guide layer".into();
            } else {
                state.ensure_undo_gesture(&doc.0);
                state.sweep_path_drag = Some(SweepPathDrag {
                    layer_id: guide_id,
                    hover: handle,
                });
                state.sweep_path_selected = Some(handle);
            }
        }
    }

    if let (Some(drag), Some(pointer)) = (state.sweep_path_drag, response.interact_pointer_pos()) {
        let hit = {
            let (origin, direction) = camera.pointer_ray(pointer);
            map.raycast(origin, direction)
        };
        if let Some(hit) = hit {
            let break_handles = ui.input(|input| input.modifiers.alt);
            if apply_sweep_handle(doc, drag.layer_id, drag.hover, hit.doc, break_handles) {
                state.mark_dirty();
                sync_operation_preview(state, doc);
                state.sweep_path_readout = format!(
                    "x {:.1}  y {:.1}  ·  UV {:.3}  {:.3}",
                    hit.doc.x, hit.doc.y, hit.uv.x, hit.uv.y
                );
            }
        } else if !state.sweep_path_readout.is_empty()
            && !state.sweep_path_readout.contains("off surface")
        {
            state.sweep_path_readout.push_str(" · off surface");
        }
    }

    if response.drag_stopped_by(egui::PointerButton::Primary) {
        state.sweep_path_drag = None;
    }
    if response.clicked() {
        if let Some(handle) = hovered {
            state.sweep_path_selected = Some(handle);
        }
    }

    SweepPathInput {
        dragging: state.sweep_path_drag.is_some(),
        hovering: hovered,
    }
}

/// Draw the guide spine, anchors and Bezier handles over the model.
pub(crate) fn paint_sweep_path_gizmo(
    painter: &egui::Painter,
    rect: egui::Rect,
    state: &EditorState,
    doc: &ns_core::Document,
    mesh_import: &MeshImport,
    input: &SweepPathInput,
) {
    let Some(guide_id) = sweep_path_target(state, doc) else {
        return;
    };
    let Some(path) = guide_path(doc, guide_id) else {
        return;
    };
    if mesh_import.surface.is_empty() {
        return;
    }
    let map = SurfaceMap::new(mesh_import, Vec2::new(doc.size.x as f32, doc.size.y as f32));
    let camera = camera_for(rect, state);

    // Spine: flatten the path exactly as the sweep does, then map every sample
    // to the surface. A sample outside the UV islands starts a new run.
    for (points, _closed) in ns_core::path::flatten_path_subpaths(path) {
        let mut run: Vec<egui::Pos2> = Vec::new();
        for point in points {
            match map.project(&camera, point) {
                Some(screen) => run.push(screen),
                None => {
                    if run.len() >= 2 {
                        painter.add(egui::Shape::dashed_line(
                            &run,
                            egui::Stroke::new(1.5_f32, ACCENT_TEXT),
                            5.0,
                            4.0,
                        ));
                    }
                    run.clear();
                }
            }
        }
        if run.len() >= 2 {
            painter.add(egui::Shape::dashed_line(
                &run,
                egui::Stroke::new(1.5_f32, ACCENT_TEXT),
                5.0,
                4.0,
            ));
        }
    }

    let handles = handle_positions(path, &map, &camera);
    let hovered = input.hovering;
    let selected = state.sweep_path_selected;
    for (handle, screen) in &handles {
        let is_hovered = hovered == Some(*handle);
        let is_selected = selected == Some(*handle);
        match handle.kind {
            NodeDragKind::Move => {
                // Anchor: white core with an accent outline, like the 2D node
                // editor, but sized for a 3D viewport.
                let size = if is_hovered {
                    ANCHOR_SIZE + 2.0
                } else {
                    ANCHOR_SIZE
                };
                let rect = egui::Rect::from_center_size(*screen, egui::Vec2::splat(size));
                let fill = if is_hovered || is_selected {
                    PRIMARY
                } else {
                    egui::Color32::WHITE
                };
                painter.rect_filled(rect, 1.0, fill);
                painter.rect_stroke(
                    rect,
                    1.0,
                    egui::Stroke::new(1.0_f32, PRIMARY),
                    egui::StrokeKind::Outside,
                );
            }
            NodeDragKind::HandleIn | NodeDragKind::HandleOut => {
                // Handle line back to the anchor is drawn below so it never
                // covers the anchor marker.
                let radius = if is_hovered {
                    HANDLE_SIZE * 0.5 + 1.5
                } else {
                    HANDLE_SIZE * 0.5
                };
                painter.circle_filled(
                    *screen,
                    radius,
                    if is_hovered || is_selected {
                        PRIMARY
                    } else {
                        egui::Color32::WHITE
                    },
                );
                painter.circle_stroke(*screen, radius, egui::Stroke::new(1.0_f32, PRIMARY));
            }
            NodeDragKind::PullHandles => {}
        }
    }

    // Handle lines connect each anchor to its tips.
    for subpath in &path.subpaths {
        for node in &subpath.nodes {
            let Some(anchor) = map.project(&camera, node.pos) else {
                continue;
            };
            for offset in [node.handle_in, node.handle_out] {
                if offset.length_squared() < 1e-4 {
                    continue;
                }
                if let Some(tip) = map.project(&camera, node.pos + offset) {
                    painter.line_segment([anchor, tip], egui::Stroke::new(1.0_f32, PRIMARY));
                }
            }
        }
    }
}

/// Bottom-left hint while the editor is visible, including the precision
/// readout after a surface hit.
pub(crate) fn sweep_path_hint(state: &EditorState) -> String {
    if !state.sweep_path_edit {
        return String::new();
    }
    if state.sweep_path_readout.is_empty() {
        "sweep path · surface snap · Alt breaks handles".into()
    } else {
        // The full hint already lives in the status bar and the Properties
        // card; the viewport line is only the precision readout so it cannot
        // collide with the right-aligned lighting label.
        format!("sweep · {}", state.sweep_path_readout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::UVec2;

    fn quad_surface(uv_bottom_left: bool) -> MeshImport {
        let mut import = MeshImport::default();
        import.surface = std::sync::Arc::new(vec![SurfaceTriangle {
            positions: [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            uvs: [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
        }]);
        import.uv_bottom_left = uv_bottom_left;
        import
    }

    fn guide_layer() -> ns_core::Layer {
        let mut path = ns_core::Path::new(ns_core::FillRule::NonZero);
        let mut subpath = ns_core::SubPath::new(false);
        subpath.nodes = vec![
            Node::corner(Vec2::new(100.0, 100.0)),
            Node::corner(Vec2::new(300.0, 100.0)),
        ];
        path.subpaths.push(subpath);
        let mut layer = ns_core::Layer::shape("Sweep path", ShapeSource::Path(path));
        if let LayerKind::Shape(shape) = &mut layer.kind {
            shape.vector_role = VectorRole::Path;
        }
        layer
    }

    fn sweep_state(guide: &ns_core::Layer) -> EditorState {
        let mut state = EditorState {
            selected: Some(vec![0]),
            ..Default::default()
        };
        let mut entry = crate::operations::OperationEntry::new(OperationKind::Sweep);
        entry.sweep.path = Some(guide.id);
        state.operation_stacks.insert(guide.id, vec![entry]);
        state
    }

    #[test]
    fn document_and_surface_uvs_round_trip_both_origins() {
        for bottom_left in [false, true] {
            let import = quad_surface(bottom_left);
            let map = SurfaceMap::new(&import, Vec2::splat(400.0));
            let point = Vec2::new(100.0, 300.0);
            let uv = map.doc_to_uv(point);
            let back = map.uv_to_doc(uv);
            assert!(
                (back - point).length() < 1e-3,
                "origin {bottom_left}: {point} -> {uv} -> {back}"
            );
        }
    }

    #[test]
    fn a_ray_through_a_surface_point_returns_the_same_document_position() {
        let import = quad_surface(false);
        let map = SurfaceMap::new(&import, Vec2::splat(400.0));
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let camera = GizmoCamera::new(rect, 0.0, 0.0, 3.0);
        let doc = Vec2::new(120.0, 260.0);
        let screen = map.project(&camera, doc).expect("point projects");
        let (origin, direction) = camera.pointer_ray(screen);
        let hit = map.raycast(origin, direction).expect("ray hits the quad");
        assert!((hit.doc - doc).length() < 0.05, "{:?} vs {doc:?}", hit.doc);
    }

    #[test]
    fn moving_one_anchor_never_resizes_the_swept_source() {
        let guide = guide_layer();
        let guide_id = guide.id;
        let mut stud = ns_core::Layer::shape(
            "stud",
            ShapeSource::Primitive(ns_core::Primitive::Rect {
                center: Vec2::new(200.0, 150.0),
                size: Vec2::splat(40.0),
                corner_radius: 2.0,
            }),
        );
        stud.visible = true;
        let mut doc = DocumentRes::new(ns_core::Document::new(UVec2::splat(400)));
        doc.0.layers.push(stud.clone());
        doc.0.layers.push(guide);
        let before = match &stud.kind {
            LayerKind::Shape(shape) => shape.source.bounds().expect("source bounds"),
            _ => unreachable!(),
        };
        let handle = SweepPathHover {
            node: NodeRef {
                subpath: 0,
                node: 1,
            },
            kind: NodeDragKind::Move,
        };
        assert!(apply_sweep_handle(
            &mut doc,
            guide_id,
            handle,
            Vec2::new(320.0, 220.0),
            false
        ));
        let after = match &stud.kind {
            LayerKind::Shape(shape) => shape.source.bounds().expect("source bounds"),
            _ => unreachable!(),
        };
        assert_eq!(before.min, after.min, "the stud must not stretch or move");
        assert_eq!(before.max, after.max);
        let path = guide_path(&doc.0, guide_id).expect("guide path");
        assert_eq!(path.subpaths[0].nodes[1].pos, Vec2::new(320.0, 220.0));
    }

    #[test]
    fn dragging_a_out_handle_mirrors_a_symmetric_in_handle() {
        let mut layer = guide_layer();
        if let LayerKind::Shape(shape) = &mut layer.kind {
            if let ShapeSource::Path(path) = &mut shape.source {
                path.subpaths[0].nodes[0].kind = NodeKind::Symmetric;
                path.subpaths[0].nodes[0].handle_out = Vec2::new(20.0, 0.0);
                path.subpaths[0].nodes[0].handle_in = Vec2::new(-20.0, 0.0);
            }
        }
        let guide_id = layer.id;
        let mut doc = DocumentRes::new(ns_core::Document::new(UVec2::splat(400)));
        doc.0.layers.push(layer);
        assert!(apply_sweep_handle(
            &mut doc,
            guide_id,
            SweepPathHover {
                node: NodeRef {
                    subpath: 0,
                    node: 0,
                },
                kind: NodeDragKind::HandleOut,
            },
            Vec2::new(140.0, 140.0),
            false
        ));
        let path = guide_path(&doc.0, guide_id).expect("guide path");
        let node = &path.subpaths[0].nodes[0];
        assert!(node.handle_out.x > 0.0 && node.handle_out.y > 0.0);
        assert!((node.handle_in + node.handle_out).length() < 1e-3);
    }

    #[test]
    fn only_an_enabled_sweep_exposes_the_path() {
        let guide = guide_layer();
        let mut doc = ns_core::Document::new(UVec2::splat(400));
        let mut state = sweep_state(&guide);
        doc.layers.push(guide.clone());
        assert_eq!(sweep_path_target(&state, &doc), Some(guide.id));
        state.operation_stacks.get_mut(&guide.id).unwrap()[0].enabled = false;
        assert_eq!(sweep_path_target(&state, &doc), None);
    }
}
