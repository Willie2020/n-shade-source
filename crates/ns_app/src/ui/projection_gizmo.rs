//! Interactive projection cage for the 3D viewport.
//!
//! The cage is drawn in screen space over the deterministic preview camera and
//! hit-tested in egui, the same way the light gizmos work. Dragging the centre
//! moves the projection box in the camera plane, the coloured arrows slide it
//! along one mesh axis, the corner square scales it and the ring twists it.
//!
//! The box is only the triplanar projection volume: nothing in the 2D document
//! changes until **Apply** bakes the projected normals back into a layer.

use super::*;
use crate::projection::{ProjectionHandle, ProjectionRuntime};
use crate::ui::lightmap_gizmo::camera_for;
use glam::{Vec2, Vec3};

const HANDLE_RADIUS: f32 = 8.0;
const CENTER_RADIUS: f32 = 9.0;
const ROTATE_RADIUS: f32 = 64.0;
/// How far past the box face the axis arrows sit, in normalized mesh units.
const AXIS_REACH: f32 = 0.09;

/// Handles are drawn as eight box corners; these pairs are the twelve edges.
const CORNERS: [(f32, f32, f32); 8] = [
    (-1.0, -1.0, -1.0),
    (1.0, -1.0, -1.0),
    (-1.0, 1.0, -1.0),
    (1.0, 1.0, -1.0),
    (-1.0, -1.0, 1.0),
    (1.0, -1.0, 1.0),
    (-1.0, 1.0, 1.0),
    (1.0, 1.0, 1.0),
];
const EDGES: [(usize, usize); 12] = [
    (0, 1),
    (2, 3),
    (4, 5),
    (6, 7),
    (0, 2),
    (1, 3),
    (4, 6),
    (5, 7),
    (0, 4),
    (1, 5),
    (2, 6),
    (3, 7),
];

#[derive(Default)]
pub(crate) struct ProjectionGizmoInput {
    /// A cage handle owns the primary drag, so orbit and layer gizmos must not
    /// also run this frame.
    pub dragging: bool,
    pub hovering: Option<ProjectionHandle>,
}

pub(crate) struct ProjectionActions {
    pub apply: bool,
    pub cancel: bool,
}

impl Default for ProjectionActions {
    fn default() -> Self {
        Self {
            apply: false,
            cancel: false,
        }
    }
}

fn axis_unit(index: usize) -> Vec3 {
    match index {
        0 => Vec3::X,
        1 => Vec3::Y,
        _ => Vec3::Z,
    }
}

fn axis_color(index: usize) -> egui::Color32 {
    match index {
        0 => AXIS_X,
        1 => AXIS_Y,
        _ => AXIS_Z,
    }
}

fn axis_handle_point(runtime: &ProjectionRuntime, index: usize) -> Vec3 {
    let half = runtime.size * 0.5;
    runtime.center
        + runtime.rotation()
            * (axis_unit(index) * (half.max_element() + AXIS_REACH).max(half[index] + AXIS_REACH))
}

fn scale_handle_point(runtime: &ProjectionRuntime) -> Vec3 {
    runtime.center + runtime.rotation() * (runtime.size * 0.5)
}

fn box_corner(runtime: &ProjectionRuntime, corner: (f32, f32, f32)) -> Vec3 {
    let local = Vec3::new(corner.0, corner.1, corner.2) * runtime.size * 0.5;
    runtime.center + runtime.rotation() * local
}

fn handle_targets(
    camera: &crate::ui::lightmap_gizmo::GizmoCamera,
    runtime: &ProjectionRuntime,
) -> Vec<(ProjectionHandle, egui::Pos2, f32)> {
    let mut out = Vec::new();
    if let Some(point) = camera.project(runtime.center) {
        out.push((ProjectionHandle::Move, point, CENTER_RADIUS));
    }
    for (index, handle) in [
        ProjectionHandle::AxisX,
        ProjectionHandle::AxisY,
        ProjectionHandle::AxisZ,
    ]
    .into_iter()
    .enumerate()
    {
        if let Some(point) = camera.project(axis_handle_point(runtime, index)) {
            out.push((handle, point, HANDLE_RADIUS));
        }
    }
    if let Some(point) = camera.project(scale_handle_point(runtime)) {
        out.push((ProjectionHandle::Scale, point, HANDLE_RADIUS));
    }
    out
}

fn pick(
    camera: &crate::ui::lightmap_gizmo::GizmoCamera,
    runtime: &ProjectionRuntime,
    pointer: egui::Pos2,
) -> Option<ProjectionHandle> {
    let targets = handle_targets(camera, runtime);
    if let Some((handle, _, _)) = targets
        .iter()
        .filter(|(_, screen, radius)| screen.distance(pointer) <= *radius)
        .min_by(|a, b| a.1.distance(pointer).total_cmp(&b.1.distance(pointer)))
    {
        return Some(*handle);
    }
    let center = camera.project(runtime.center)?;
    let ring = (pointer.distance(center) - ROTATE_RADIUS).abs();
    (ring <= HANDLE_RADIUS).then_some(ProjectionHandle::Rotate)
}

/// Handle a drag that starts on the projection cage. The caller skips orbit and
/// layer-gizmo input while `dragging` is true.
pub(crate) fn projection_gizmo_input(
    ui: &egui::Ui,
    response: &egui::Response,
    rect: egui::Rect,
    state: &EditorState,
    runtime: &mut ProjectionRuntime,
) -> ProjectionGizmoInput {
    if !runtime.enabled {
        runtime.drag_handle = None;
        return ProjectionGizmoInput::default();
    }
    let camera = camera_for(rect, state);
    let pointer = response.hover_pos();
    let hovering = pointer.and_then(|pointer| pick(&camera, runtime, pointer));

    if response.drag_stopped() {
        runtime.drag_handle = None;
    }
    if response.drag_started_by(egui::PointerButton::Primary) {
        let press = ui
            .input(|input| input.pointer.press_origin())
            .or_else(|| response.interact_pointer_pos());
        if let Some(press) = press {
            if let Some(handle) = pick(&camera, runtime, press) {
                runtime.drag_handle = Some(handle);
                runtime.drag_start = Vec2::new(press.x, press.y);
                runtime.drag_start_center = runtime.center;
                runtime.drag_start_size = runtime.size;
                runtime.drag_start_rotation = runtime.rotation_deg;
                if let Some(center) = camera.project(runtime.center) {
                    let relative = press - center;
                    runtime.drag_start_distance = relative.length().max(1.0);
                    runtime.drag_start_angle = relative.y.atan2(relative.x);
                }
            }
        }
    }

    let mut dragging = false;
    if let Some(handle) = runtime.drag_handle {
        if response.dragged_by(egui::PointerButton::Primary) {
            let pointer = response
                .interact_pointer_pos()
                .or(pointer)
                .unwrap_or(egui::pos2(runtime.drag_start.x, runtime.drag_start.y));
            apply_drag(runtime, handle, &camera, pointer);
            dragging = true;
        }
    }

    if dragging {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    } else if hovering.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    ProjectionGizmoInput { dragging, hovering }
}

fn apply_drag(
    runtime: &mut ProjectionRuntime,
    handle: ProjectionHandle,
    camera: &crate::ui::lightmap_gizmo::GizmoCamera,
    pointer: egui::Pos2,
) {
    let current = Vec2::new(pointer.x, pointer.y);
    let delta = current - runtime.drag_start;
    let screen_delta = egui::vec2(delta.x, delta.y);
    let center = runtime.drag_start_center;
    let rotation = runtime.rotation();
    match handle {
        ProjectionHandle::Move => {
            runtime.center = center + camera.screen_delta(center, screen_delta);
        }
        ProjectionHandle::AxisX | ProjectionHandle::AxisY | ProjectionHandle::AxisZ => {
            let axis = match handle {
                ProjectionHandle::AxisX => rotation * Vec3::X,
                ProjectionHandle::AxisY => rotation * Vec3::Y,
                _ => rotation * Vec3::Z,
            };
            let world = camera.screen_delta(center, screen_delta);
            runtime.center = center + axis * world.dot(axis);
        }
        ProjectionHandle::Scale => {
            let Some(center_screen) = camera.project(center) else {
                return;
            };
            let before = Vec2::new(
                runtime.drag_start.x - center_screen.x,
                runtime.drag_start.y - center_screen.y,
            )
            .length()
            .max(1.0);
            let after = Vec2::new(pointer.x - center_screen.x, pointer.y - center_screen.y)
                .length()
                .max(1.0);
            let factor = (after / before).clamp(0.05, 20.0);
            runtime.size =
                (runtime.drag_start_size * factor).clamp(Vec3::splat(0.005), Vec3::splat(8.0));
        }
        ProjectionHandle::Rotate => {
            let Some(center_screen) = camera.project(center) else {
                return;
            };
            let angle = (pointer.y - center_screen.y).atan2(pointer.x - center_screen.x);
            runtime.rotation_deg.z =
                runtime.drag_start_rotation.z + (angle - runtime.drag_start_angle).to_degrees();
        }
    }
    runtime.mark_dirty();
}

/// Draw the cage, axes, handles and the twist ring.
pub(crate) fn paint_projection_gizmo(
    painter: &egui::Painter,
    rect: egui::Rect,
    state: &EditorState,
    runtime: &ProjectionRuntime,
    hovering: Option<ProjectionHandle>,
) {
    let camera = camera_for(rect, state);
    let Some(center) = camera.project(runtime.center) else {
        return;
    };
    let hovered = |handle: ProjectionHandle| hovering == Some(handle);

    // Box edges, slightly translucent so the model stays readable.
    let cage = ACCENT.gamma_multiply(0.85);
    let mut projected = [None; 8];
    for (index, corner) in CORNERS.iter().enumerate() {
        projected[index] = camera.project(box_corner(runtime, *corner));
    }
    for (a, b) in EDGES {
        if let (Some(a), Some(b)) = (projected[a], projected[b]) {
            painter.line_segment([a, b], egui::Stroke::new(1.2_f32, cage));
        }
    }

    // Move axes.
    for index in 0..3 {
        let color = if hovered(match index {
            0 => ProjectionHandle::AxisX,
            1 => ProjectionHandle::AxisY,
            _ => ProjectionHandle::AxisZ,
        }) {
            egui::Color32::WHITE
        } else {
            axis_color(index)
        };
        if let Some(end) = camera.project(axis_handle_point(runtime, index)) {
            painter.line_segment(
                [center, end],
                egui::Stroke::new(
                    if hovered(ProjectionHandle::Move) {
                        2.4_f32
                    } else {
                        1.6
                    },
                    color,
                ),
            );
            painter.circle_filled(end, HANDLE_RADIUS * 0.8, color);
            painter.text(
                end + egui::vec2(9.0, -9.0),
                egui::Align2::LEFT_BOTTOM,
                ["X", "Y", "Z"][index],
                egui::FontId::monospace(10.0),
                color,
            );
        }
    }

    // Uniform scale handle at the +X/+Y/+Z corner.
    if let Some(scale) = camera.project(scale_handle_point(runtime)) {
        let color = if hovered(ProjectionHandle::Scale) {
            egui::Color32::WHITE
        } else {
            ACCENT
        };
        painter.rect_filled(
            egui::Rect::from_center_size(scale, egui::vec2(10.0, 10.0)),
            1.0,
            color,
        );
    }

    // Centre move handle.
    let center_color = if hovered(ProjectionHandle::Move) {
        egui::Color32::WHITE
    } else {
        ACCENT
    };
    painter.circle_filled(center, 5.0, center_color);
    painter.circle_stroke(center, CENTER_RADIUS, egui::Stroke::new(1.0, center_color));

    // Twist ring around the view axis.
    let ring_color = if hovered(ProjectionHandle::Rotate) {
        egui::Color32::WHITE
    } else {
        PRIMARY_COOL
    };
    painter.circle_stroke(
        center,
        ROTATE_RADIUS,
        egui::Stroke::new(1.5_f32, ring_color.gamma_multiply(0.8)),
    );

    if let Some(handle) = hovering {
        let hint = match handle {
            ProjectionHandle::Move => "Move · drag in the view plane",
            ProjectionHandle::AxisX => "Move along X",
            ProjectionHandle::AxisY => "Move along Y",
            ProjectionHandle::AxisZ => "Move along Z",
            ProjectionHandle::Scale => "Scale uniformly",
            ProjectionHandle::Rotate => "Rotate around the projection axis",
        };
        painter.text(
            rect.left_bottom() + egui::vec2(10.0, -8.0),
            egui::Align2::LEFT_BOTTOM,
            hint,
            egui::FontId::proportional(TEXT_SMALL),
            ACCENT_TEXT,
        );
    }
}

fn axis_field(
    ui: &mut egui::Ui,
    color: egui::Color32,
    label: &str,
    value: &mut f32,
    speed: f32,
) -> bool {
    ui.colored_label(color, label);
    let response = ui.add_sized(
        [38.0, 18.0],
        egui::DragValue::new(value).speed(speed).max_decimals(3),
    );
    response.changed()
}

fn vec3_row(ui: &mut egui::Ui, label: &str, value: &mut Vec3, speed: f32) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        ui.label(egui::RichText::new(label).size(TEXT_CAPTION).color(MUTED));
        changed |= axis_field(ui, AXIS_X, "X", &mut value.x, speed);
        changed |= axis_field(ui, AXIS_Y, "Y", &mut value.y, speed);
        changed |= axis_field(ui, AXIS_Z, "Z", &mut value.z, speed);
    });
    changed
}

/// The projection controls card, drawn in the Properties dock while the tool
/// is active. Returns Apply/Cancel for the caller to run after the UI closes.
pub(crate) fn projection_card(
    ui: &mut egui::Ui,
    runtime: &mut ProjectionRuntime,
) -> ProjectionActions {
    let mut actions = ProjectionActions::default();
    ui.add_space(8.0);
    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        let state_pill = if runtime.is_busy() { "Updating" } else { "Live" };
        section_title_with_pill(
            ui,
            Icon::Projection,
            "3D Projection",
            Some((state_pill, true)),
        );
        ui.label(
            egui::RichText::new(
                "Cube projection · drive the cage on the model, then Apply bakes it into the 2D canvas.",
            )
            .size(TEXT_CAPTION)
            .color(MUTED),
        );
        ui.add_space(4.0);

        let mut changed = false;
        changed |= vec3_row(ui, "Move", &mut runtime.center, 0.005);
        changed |= vec3_row(ui, "Size", &mut runtime.size, 0.005);
        runtime.size = runtime.size.clamp(Vec3::splat(0.005), Vec3::splat(8.0));
        changed |= vec3_row(ui, "Rotate", &mut runtime.rotation_deg, 0.5);
        changed |= slider_row(ui, "Sharp", &mut runtime.sharpness, 1.0..=12.0, 0.01, 1);
        changed |= slider_row(ui, "Feather", &mut runtime.feather, 0.0..=0.25, 0.001, 3);
        if changed {
            runtime.mark_dirty();
        }

        if let Some(error) = &runtime.error {
            ui.label(
                egui::RichText::new(error.as_str())
                    .size(TEXT_CAPTION)
                    .color(WARNING),
            );
        } else if !runtime.status.is_empty() {
            ui.label(
                egui::RichText::new(runtime.status.as_str())
                    .size(TEXT_CAPTION)
                    .color(MUTED),
            );
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui
                .button("Fit")
                .on_hover_text("Fit the cage to the mesh bounds")
                .clicked()
            {
                runtime.fit_to_mesh();
            }
            if ui
                .button("Reset")
                .on_hover_text("Reset the cage to a unit box")
                .clicked()
            {
                runtime.reset_bounds();
            }
            if ui.button("Cancel").clicked() {
                actions.cancel = true;
            }
        });
        if filled_button(ui, "Apply to 2D", 28.0)
            .on_hover_text("Bake the projected normals into a new layer above the source")
            .clicked()
        {
            actions.apply = true;
        }
    });
    if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
        actions.cancel = true;
    }
    actions
}
