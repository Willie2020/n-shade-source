//! Interactive light gizmos over the 3D preview.
//!
//! The bake reads its lighting from `LightmapSettings`, which live outside the
//! 2D document. These gizmos are the 3D counterpart of the Lightmap panel's
//! scene-light rows: they project the sun and every point/spot/area light into
//! the preview camera, draw body/aim/range handles, and translate a screen drag
//! back into normalized mesh space.
//!
//! The preview camera is deterministic (orbit yaw/pitch/distance around the
//! origin), so the projection is computed here and the egui layer that owns the
//! viewport input does the hit-testing. No Bevy picking is involved.

use super::*;
use glam::{Quat, Vec3};
use ns_bake::{LightmapLightKind, LightmapSceneLight, LightmapSettings};

/// Distance from the origin used to place the directional sun icon.
const SUN_GIZMO_DISTANCE: f32 = 1.55;
/// Opacity of a light whose Preview switch is off.
const HIDDEN_ALPHA: f32 = 0.35;
/// Screen radius of a light body icon, in points.
const ICON_RADIUS: f32 = 11.0;
/// Screen radius of a spot/area aim handle, in points.
const TARGET_RADIUS: f32 = 9.0;
/// Length of the direction arrow drawn from a spot/area light, in normalized
/// mesh units. Short enough to stay near the light, long enough to grab.
const AIM_LENGTH: f32 = 0.38;
/// Screen radius of the rotation ring around the selected light.
const RING_RADIUS: f32 = 30.0;
/// How close the pointer must be to the ring to grab it, in points.
const RING_GRAB: f32 = 7.0;

#[derive(Default)]
pub(crate) struct LightGizmoInput {
    /// The primary drag is moving a light, so orbit/layer drags must not run.
    pub dragging: bool,
    /// Icon under the pointer, for the cursor and the hover ring.
    pub hovering: Option<LightSelection>,
}

/// Perspective camera matching [`crate::preview::apply_camera_viewport`].
///
/// `yaw`, `pitch` and `distance` are the same orbit values the 3D header
/// exposes; the projection only needs the vertical FOV and the pane aspect.
#[derive(Clone, Copy)]
pub(crate) struct GizmoCamera {
    rect: egui::Rect,
    eye: Vec3,
    forward: Vec3,
    right: Vec3,
    up: Vec3,
    tan_half: f32,
    aspect: f32,
}

impl GizmoCamera {
    pub(crate) fn new(rect: egui::Rect, yaw: f32, pitch: f32, distance: f32) -> Self {
        let (forward, right, up) = camera_basis(yaw, pitch);
        let eye = -forward * distance.max(0.1);
        let aspect = (rect.width() / rect.height().max(1.0)).max(0.01);
        Self {
            rect,
            eye,
            forward,
            right,
            up,
            tan_half: (PREVIEW_FOV * 0.5).tan(),
            aspect,
        }
    }

    pub(crate) fn project(&self, point: Vec3) -> Option<egui::Pos2> {
        let relative = point - self.eye;
        let depth = relative.dot(self.forward);
        if depth <= 0.05 {
            return None;
        }
        let ndc_x = relative.dot(self.right) / (depth * self.tan_half * self.aspect);
        let ndc_y = relative.dot(self.up) / (depth * self.tan_half);
        Some(
            self.rect.center()
                + egui::vec2(
                    ndc_x * self.rect.width() * 0.5,
                    -ndc_y * self.rect.height() * 0.5,
                ),
        )
    }

    /// World units covered by one screen point at `point`'s depth.
    pub(crate) fn world_per_pixel(&self, point: Vec3) -> f32 {
        let depth = (point - self.eye).dot(self.forward).max(0.25);
        2.0 * depth * self.tan_half / self.rect.height().max(1.0)
    }

    pub(crate) fn screen_delta(&self, point: Vec3, delta: egui::Vec2) -> Vec3 {
        let scale = self.world_per_pixel(point);
        self.right * (delta.x * scale) - self.up * (delta.y * scale)
    }

    /// Ray through a screen point, in the camera's world space. Used by the
    /// sweep-path editor to hit the preview mesh under the pointer.
    pub(crate) fn pointer_ray(&self, screen: egui::Pos2) -> (Vec3, Vec3) {
        let x = (screen.x - self.rect.center().x) / (self.rect.width() * 0.5).max(1e-3);
        let y = -(screen.y - self.rect.center().y) / (self.rect.height() * 0.5).max(1e-3);
        let direction = (self.forward
            + self.right * (x * self.tan_half * self.aspect)
            + self.up * (y * self.tan_half))
            .normalize_or(self.forward);
        (self.eye, direction)
    }
}

#[derive(Clone, Copy)]
struct GizmoTarget {
    selection: LightSelection,
    handle: LightGizmoHandle,
    screen: egui::Pos2,
    radius: f32,
}

#[derive(Clone, Copy)]
struct IconStyle {
    selected: bool,
    hovered: bool,
    show_label: bool,
}

/// Light gizmos are relevant while the lighting preview is on, or while the
/// Lightmap tab is open and the rig is being authored. The Lighting card can
/// turn the whole rig off.
pub(crate) fn light_gizmos_visible(state: &EditorState, runtime: &LightmapRuntime) -> bool {
    state.show_light_gizmos && (runtime.preview_enabled || state.right_tab == RightTab::Lightmap)
}

pub(crate) fn camera_for(rect: egui::Rect, state: &EditorState) -> GizmoCamera {
    GizmoCamera::new(
        rect,
        state.orbit_yaw,
        state.orbit_pitch,
        state.orbit_distance,
    )
}

fn sun_icon_position(settings: &LightmapSettings) -> Vec3 {
    let direction = settings.sun_direction();
    if direction.length_squared() <= 1e-8 {
        Vec3::Y
    } else {
        direction.normalize() * SUN_GIZMO_DISTANCE
    }
}

/// Unit direction a spot or area light emits along.
fn aim_direction(light: &LightmapSceneLight) -> Vec3 {
    (Vec3::from_array(light.target) - Vec3::from_array(light.position)).normalize_or_zero()
}

/// Direction handle position: a fixed distance along the emission axis so the
/// handle stays near the light instead of running to the aim target.
fn aim_handle_point(light: &LightmapSceneLight) -> Option<Vec3> {
    let direction = aim_direction(light);
    (direction.length_squared() > 1e-8)
        .then(|| Vec3::from_array(light.position) + direction * AIM_LENGTH)
}

fn gizmo_targets(
    rect: egui::Rect,
    state: &EditorState,
    runtime: &LightmapRuntime,
) -> Vec<GizmoTarget> {
    let camera = camera_for(rect, state);
    let mut targets = Vec::new();
    if let Some((screen, _)) = project_to_view(&camera, rect, sun_icon_position(&runtime.settings))
    {
        targets.push(GizmoTarget {
            selection: LightSelection::Sun,
            handle: LightGizmoHandle::Sun,
            screen,
            radius: ICON_RADIUS + 2.0,
        });
    }
    for (index, light) in runtime.settings.lights.iter().enumerate() {
        if let Some((screen, _)) = project_to_view(&camera, rect, Vec3::from_array(light.position))
        {
            targets.push(GizmoTarget {
                selection: LightSelection::Scene(index),
                handle: LightGizmoHandle::Body(index),
                screen,
                radius: ICON_RADIUS,
            });
        }
        if light.kind != LightmapLightKind::Point {
            if let Some(aim) = aim_handle_point(light) {
                if let Some((screen, _)) = project_to_view(&camera, rect, aim) {
                    targets.push(GizmoTarget {
                        selection: LightSelection::Scene(index),
                        handle: LightGizmoHandle::Target(index),
                        screen,
                        radius: TARGET_RADIUS,
                    });
                }
            }
        }
    }
    targets
}

/// Project a point and keep off-screen handles reachable at the pane edge.
/// The bool is true when the real projection lies outside the pane.
fn project_to_view(
    camera: &GizmoCamera,
    rect: egui::Rect,
    point: Vec3,
) -> Option<(egui::Pos2, bool)> {
    let screen = camera.project(point)?;
    // Leave room for the viewport chip at the top and the hint at the bottom.
    let limit = egui::Rect::from_min_max(
        rect.min + egui::vec2(ICON_RADIUS + 5.0, 42.0),
        rect.max - egui::vec2(ICON_RADIUS + 5.0, ICON_RADIUS + 5.0),
    );
    if limit.width() <= 1.0 || limit.height() <= 1.0 {
        return Some((screen, false));
    }
    let clamped = egui::pos2(
        screen.x.clamp(limit.left(), limit.right()),
        screen.y.clamp(limit.top(), limit.bottom()),
    );
    Some((clamped, (clamped - screen).length_sq() > 1e-4))
}

fn pick(targets: &[GizmoTarget], pointer: egui::Pos2) -> Option<GizmoTarget> {
    targets
        .iter()
        .filter(|target| target.screen.distance(pointer) <= target.radius)
        .min_by(|a, b| {
            a.screen
                .distance(pointer)
                .total_cmp(&b.screen.distance(pointer))
        })
        .copied()
}

/// The selected light's rotation ring, which drags the same way as its aim
/// handle. Rings are only active for the selected light so they never steal a
/// click meant for a nearby icon.
fn pick_ring(
    targets: &[GizmoTarget],
    pointer: egui::Pos2,
    selected: Option<LightSelection>,
) -> Option<GizmoTarget> {
    let selection = selected?;
    let body = targets.iter().find(|target| {
        target.selection == selection
            && matches!(
                target.handle,
                LightGizmoHandle::Body(_) | LightGizmoHandle::Sun
            )
    })?;
    ((body.screen.distance(pointer) - RING_RADIUS).abs() <= RING_GRAB).then_some(GizmoTarget {
        selection: body.selection,
        handle: match body.handle {
            LightGizmoHandle::Body(index) => LightGizmoHandle::Target(index),
            other => other,
        },
        screen: body.screen,
        radius: body.radius,
    })
}

/// Handle selection and dragging for one frame. Returns whether the primary
/// drag was consumed by a light handle.
pub(crate) fn light_gizmo_input(
    ui: &egui::Ui,
    response: &egui::Response,
    rect: egui::Rect,
    state: &EditorState,
    runtime: &mut LightmapRuntime,
) -> LightGizmoInput {
    if !light_gizmos_visible(state, runtime) {
        runtime.dragging_light = None;
        return LightGizmoInput::default();
    }
    let camera = camera_for(rect, state);
    let targets = gizmo_targets(rect, state, runtime);
    let selected = runtime.selected_light;
    let pick_at = |pointer: egui::Pos2| {
        pick(&targets, pointer).or_else(|| pick_ring(&targets, pointer, selected))
    };
    let hovering = response
        .hover_pos()
        .and_then(pick_at)
        .map(|target| target.selection);

    if response.drag_stopped() {
        runtime.dragging_light = None;
    }
    if response.drag_started_by(egui::PointerButton::Primary) {
        let press = ui
            .input(|input| input.pointer.press_origin())
            .or_else(|| response.interact_pointer_pos());
        if let Some(target) = press.and_then(pick_at) {
            runtime.selected_light = Some(target.selection);
            runtime.dragging_light = Some(target.handle);
        }
    }
    if response.clicked_by(egui::PointerButton::Primary) {
        runtime.selected_light = response
            .interact_pointer_pos()
            .and_then(pick_at)
            .map(|target| target.selection);
    }

    let mut dragging = false;
    if let Some(handle) = runtime.dragging_light {
        if response.dragged_by(egui::PointerButton::Primary) {
            let delta = response.drag_delta();
            if delta.length_sq() > 0.0 {
                apply_light_drag(&mut runtime.settings, handle, delta, &camera);
            }
            dragging = true;
        }
    }

    if dragging {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    } else if hovering.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    LightGizmoInput { dragging, hovering }
}

/// One drag step in normalized mesh space. Positions and aims share the same
/// bounds as the Lightmap panel's vector fields.
fn apply_light_drag(
    settings: &mut LightmapSettings,
    handle: LightGizmoHandle,
    delta: egui::Vec2,
    camera: &GizmoCamera,
) {
    let bounds = Vec3::splat(20.0);
    match handle {
        LightGizmoHandle::Sun => {
            settings.sun_azimuth_deg = (settings.sun_azimuth_deg - delta.x * 0.5).rem_euclid(360.0);
            settings.sun_elevation_deg =
                (settings.sun_elevation_deg - delta.y * 0.5).clamp(0.0, 90.0);
        }
        LightGizmoHandle::Body(index) => {
            let Some(light) = settings.lights.get(index) else {
                return;
            };
            let position = Vec3::from_array(light.position);
            let world = camera.screen_delta(position, delta);
            let new_position = (position + world).clamp(-bounds, bounds);
            let shift = new_position - position;
            let aimed = light.kind != LightmapLightKind::Point;
            let light = &mut settings.lights[index];
            light.position = new_position.to_array();
            if aimed {
                // Keep the aim rigid while moving the body; use the aim handle
                // for rotation.
                light.target = (Vec3::from_array(light.target) + shift).to_array();
            }
        }
        LightGizmoHandle::Target(index) => {
            let Some(light) = settings.lights.get_mut(index) else {
                return;
            };
            let position = Vec3::from_array(light.position);
            let target = Vec3::from_array(light.target);
            let offset = target - position;
            let distance = offset.length().max(0.05);
            let mut direction = offset.normalize_or_zero();
            if direction.length_squared() <= 1e-8 {
                direction = Vec3::NEG_Z;
            }
            // Horizontal drag orbits the aim around the view's up axis;
            // vertical drag tips it around the view's right axis. The aim
            // distance is preserved, so this rotates the light rather than
            // moving the target object.
            direction = Quat::from_axis_angle(camera.up, -delta.x * 0.01) * direction;
            direction = Quat::from_axis_angle(camera.right, -delta.y * 0.01) * direction;
            light.target = (position + direction.normalize_or_zero() * distance)
                .clamp(-bounds, bounds)
                .to_array();
        }
    }
}

/// Draw the rig over the 3D pane. `hovered` comes from [`light_gizmo_input`].
pub(crate) fn paint_light_gizmos(
    painter: &egui::Painter,
    rect: egui::Rect,
    state: &EditorState,
    runtime: &LightmapRuntime,
    hovered: Option<LightSelection>,
) {
    if !light_gizmos_visible(state, runtime) {
        return;
    }
    let camera = camera_for(rect, state);
    let selected = runtime.selected_light;

    for (index, light) in runtime.settings.lights.iter().enumerate() {
        let position = Vec3::from_array(light.position);
        let Some(body_screen) = camera.project(position) else {
            continue;
        };
        let Some((icon_screen, off_screen)) = project_to_view(&camera, rect, position) else {
            continue;
        };
        let selection = LightSelection::Scene(index);
        let is_selected = selected == Some(selection);
        let is_hovered = hovered == Some(selection);
        let alpha = if light.visible_in_view {
            1.0
        } else {
            HIDDEN_ALPHA
        };
        let color = light_color(light.color, alpha);
        let stroke = egui::Stroke::new(if is_selected || is_hovered { 1.8 } else { 1.1 }, color);
        let faint = egui::Stroke::new(1.0, color.gamma_multiply(0.55));

        // Wireframes use the true projection so a light just outside the pane
        // still draws the part of its cone or range that crosses the view. The
        // icon itself is clamped to the edge to stay clickable.
        if light.kind != LightmapLightKind::Area && light.range > 0.0 {
            let radius = (light.range / camera.world_per_pixel(position)).clamp(2.0, 2000.0);
            painter.circle_stroke(body_screen, radius, faint);
        }
        if light.kind != LightmapLightKind::Point {
            if let Some(aim) = aim_handle_point(light) {
                if let Some(target_screen) = camera.project(aim) {
                    painter.line_segment([body_screen, target_screen], stroke);
                    paint_aim_arrow(painter, target_screen, body_screen, color);
                    painter.circle_stroke(target_screen, TARGET_RADIUS - 3.0, stroke);
                    painter.circle_filled(target_screen, 1.6, color);
                    // Faint continuation toward the real aim point, which may
                    // sit far outside the mesh.
                    if let Some(actual) = camera.project(Vec3::from_array(light.target)) {
                        painter.line_segment(
                            [target_screen, actual],
                            egui::Stroke::new(1.0, color.gamma_multiply(0.25)),
                        );
                    }
                }
            }
            match light.kind {
                LightmapLightKind::Spot => {
                    paint_spot_cone(painter, &camera, light, body_screen, stroke)
                }
                LightmapLightKind::Area => paint_area_outline(painter, &camera, light, stroke),
                LightmapLightKind::Point => {}
            }
        }
        if is_selected {
            paint_rotation_ring(painter, icon_screen, color);
        }

        paint_light_icon(
            painter,
            icon_screen,
            light.kind,
            color,
            IconStyle {
                selected: is_selected,
                hovered: is_hovered,
                show_label: !off_screen,
            },
            &light.name,
        );
    }

    let sun = sun_icon_position(&runtime.settings);
    if let Some((screen, off_screen)) = project_to_view(&camera, rect, sun) {
        let is_selected = selected == Some(LightSelection::Sun);
        let is_hovered = hovered == Some(LightSelection::Sun);
        let active = runtime.settings.sun_enabled && runtime.settings.sun_intensity > 0.0;
        let color = light_color(
            runtime.settings.sun_color,
            if active { 1.0 } else { HIDDEN_ALPHA },
        );
        if let Some(origin) = camera.project(Vec3::ZERO) {
            painter.line_segment(
                [screen, origin],
                egui::Stroke::new(1.0, color.gamma_multiply(0.35)),
            );
        }
        paint_sun_icon(painter, screen, color, is_selected, is_hovered);
        if is_selected {
            paint_rotation_ring(painter, screen, color);
        }
        if !off_screen {
            paint_label(
                painter,
                screen,
                "Sun",
                if is_selected { ACCENT_TEXT } else { TEXT },
                is_selected || is_hovered,
            );
        }
    }
}

fn light_color(rgb: [f32; 3], alpha: f32) -> egui::Color32 {
    // Lift very dark light colours so the gizmo stays visible against the
    // viewport, without changing the light itself.
    let brightest = rgb.iter().copied().fold(0.0_f32, f32::max).max(1e-5);
    let lift = (0.45 / brightest).clamp(1.0, 6.0);
    let channel = |value: f32| ((value * lift).clamp(0.0, 1.0) * 255.0).round() as u8;
    egui::Color32::from_rgba_unmultiplied(
        channel(rgb[0]),
        channel(rgb[1]),
        channel(rgb[2]),
        (alpha.clamp(0.0, 1.0) * 255.0).round() as u8,
    )
}

fn paint_light_icon(
    painter: &egui::Painter,
    screen: egui::Pos2,
    kind: LightmapLightKind,
    color: egui::Color32,
    style: IconStyle,
    name: &str,
) {
    let IconStyle {
        selected,
        hovered,
        show_label,
    } = style;
    if selected || hovered {
        painter.circle_stroke(
            screen,
            ICON_RADIUS + 3.0,
            egui::Stroke::new(
                if selected { 2.0 } else { 1.2 },
                if selected {
                    ACCENT
                } else {
                    egui::Color32::WHITE
                },
            ),
        );
    }
    match kind {
        LightmapLightKind::Point => {
            painter.circle_filled(screen, 4.5, color);
            painter.circle_stroke(
                screen,
                4.5,
                egui::Stroke::new(1.0, egui::Color32::from_black_alpha(150)),
            );
        }
        LightmapLightKind::Spot => {
            let points = vec![
                screen + egui::vec2(0.0, -6.0),
                screen + egui::vec2(6.0, 5.0),
                screen + egui::vec2(-6.0, 5.0),
            ];
            painter.add(egui::Shape::convex_polygon(
                points,
                color,
                egui::Stroke::NONE,
            ));
        }
        LightmapLightKind::Area => {
            let frame = egui::Rect::from_center_size(screen, egui::vec2(10.0, 8.0));
            painter.rect_filled(
                frame,
                1.5,
                egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 90),
            );
            painter.rect_stroke(
                frame,
                1.5,
                egui::Stroke::new(1.4, color),
                egui::StrokeKind::Inside,
            );
        }
    }
    if show_label {
        paint_label(
            painter,
            screen,
            name,
            if selected { ACCENT_TEXT } else { TEXT },
            selected || hovered,
        );
    }
}

fn paint_sun_icon(
    painter: &egui::Painter,
    screen: egui::Pos2,
    color: egui::Color32,
    selected: bool,
    hovered: bool,
) {
    if selected || hovered {
        painter.circle_stroke(
            screen,
            ICON_RADIUS + 4.0,
            egui::Stroke::new(
                if selected { 2.0 } else { 1.2 },
                if selected {
                    ACCENT
                } else {
                    egui::Color32::WHITE
                },
            ),
        );
    }
    painter.circle_filled(screen, 4.5, color);
    for index in 0..8 {
        let angle = index as f32 / 8.0 * std::f32::consts::TAU;
        let direction = egui::vec2(angle.cos(), angle.sin());
        painter.line_segment(
            [screen + direction * 6.5, screen + direction * 9.5],
            egui::Stroke::new(1.2, color),
        );
    }
}

/// Screen-space arrowhead at the aim handle, pointing away from the light.
fn paint_aim_arrow(
    painter: &egui::Painter,
    tip: egui::Pos2,
    body: egui::Pos2,
    color: egui::Color32,
) {
    let along = tip - body;
    if along.length_sq() <= 1e-4 {
        return;
    }
    let along = along.normalized();
    let side = egui::vec2(-along.y, along.x);
    let base = tip - along * 7.0;
    painter.add(egui::Shape::convex_polygon(
        vec![tip, base + side * 3.6, base - side * 3.6],
        color,
        egui::Stroke::NONE,
    ));
}

/// Rotation ring around the selected light, matching the projection cage's
/// twist handle.
fn paint_rotation_ring(painter: &egui::Painter, centre: egui::Pos2, color: egui::Color32) {
    painter.circle_stroke(
        centre,
        RING_RADIUS,
        egui::Stroke::new(1.0, color.gamma_multiply(0.55)),
    );
    for index in 0..4 {
        let angle = index as f32 * std::f32::consts::FRAC_PI_2;
        let direction = egui::vec2(angle.cos(), angle.sin());
        painter.line_segment(
            [
                centre + direction * (RING_RADIUS - 3.0),
                centre + direction * (RING_RADIUS + 3.0),
            ],
            egui::Stroke::new(1.2, color.gamma_multiply(0.75)),
        );
    }
}

fn paint_label(
    painter: &egui::Painter,
    screen: egui::Pos2,
    name: &str,
    color: egui::Color32,
    strong: bool,
) {
    if name.is_empty() {
        return;
    }
    let galley = painter.layout_no_wrap(
        name.to_owned(),
        egui::FontId::proportional(TEXT_SMALL),
        color,
    );
    let origin = screen + egui::vec2(ICON_RADIUS + 4.0, -galley.size().y * 0.5);
    let chip = egui::Rect::from_min_size(
        origin - egui::vec2(4.0, 2.0),
        galley.size() + egui::vec2(8.0, 4.0),
    );
    painter.rect_filled(
        chip,
        3.0,
        egui::Color32::from_black_alpha(if strong { 170 } else { 110 }),
    );
    painter.galley(origin, galley, color);
}

fn paint_spot_cone(
    painter: &egui::Painter,
    camera: &GizmoCamera,
    light: &LightmapSceneLight,
    screen: egui::Pos2,
    stroke: egui::Stroke,
) {
    let position = Vec3::from_array(light.position);
    let target = Vec3::from_array(light.target);
    let direction = (target - position).normalize_or_zero();
    if direction.length_squared() <= 1e-8 {
        return;
    }
    let length = if light.range > 0.0 {
        light.range.min(0.9)
    } else {
        0.65
    };
    let half_angle = (light.spot_angle_deg * 0.5).to_radians().clamp(0.01, 1.45);
    let radius = length * half_angle.tan();
    let right = if direction.cross(Vec3::Y).length_squared() > 1e-6 {
        direction.cross(Vec3::Y).normalize()
    } else {
        direction.cross(Vec3::X).normalize()
    };
    let up = right.cross(direction).normalize_or_zero();
    let end = position + direction * length;

    let mut rim = Vec::with_capacity(16);
    let mut ribs = Vec::new();
    for index in 0..16 {
        let angle = index as f32 / 16.0 * std::f32::consts::TAU;
        let world = end + right * (angle.cos() * radius) + up * (angle.sin() * radius);
        if let Some(point) = camera.project(world) {
            rim.push(point);
        }
        if index % 4 == 0 {
            ribs.push(world);
        }
    }
    if rim.len() == 16 {
        painter.add(egui::Shape::closed_line(rim, stroke));
    }
    for world in ribs {
        if let Some(point) = camera.project(world) {
            painter.line_segment(
                [screen, point],
                egui::Stroke::new(stroke.width * 0.7, stroke.color.gamma_multiply(0.6)),
            );
        }
    }
}

fn paint_area_outline(
    painter: &egui::Painter,
    camera: &GizmoCamera,
    light: &LightmapSceneLight,
    stroke: egui::Stroke,
) {
    let position = Vec3::from_array(light.position);
    let target = Vec3::from_array(light.target);
    let normal = (target - position).normalize_or_zero();
    if normal.length_squared() <= 1e-8 {
        return;
    }
    let right = if normal.cross(Vec3::Y).length_squared() > 1e-6 {
        normal.cross(Vec3::Y).normalize()
    } else {
        normal.cross(Vec3::X).normalize()
    };
    let up = right.cross(normal).normalize_or_zero();
    let half_width = light.area_size[0].abs() * 0.5;
    let half_height = light.area_size[1].abs() * 0.5;
    let corners = [
        position + right * half_width + up * half_height,
        position - right * half_width + up * half_height,
        position - right * half_width - up * half_height,
        position + right * half_width - up * half_height,
    ];
    let projected: Vec<egui::Pos2> = corners
        .iter()
        .filter_map(|corner| camera.project(*corner))
        .collect();
    if projected.len() == 4 {
        let fill = egui::Color32::from_rgba_unmultiplied(
            stroke.color.r(),
            stroke.color.g(),
            stroke.color.b(),
            28,
        );
        painter.add(egui::Shape::convex_polygon(projected, fill, stroke));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0))
    }

    fn camera() -> GizmoCamera {
        GizmoCamera::new(rect(), 0.0, 0.0, 3.0)
    }

    fn light_settings(kind: LightmapLightKind) -> LightmapSettings {
        let mut settings = LightmapSettings::default();
        settings.lights.push(LightmapSceneLight::new(kind));
        settings
    }

    #[test]
    fn camera_projects_the_scene_axes() {
        let camera = camera();
        let center = camera.project(Vec3::ZERO).expect("origin in front");
        assert!((center - rect().center()).length() < 1e-3);
        let right = camera.project(Vec3::X).expect("x in front");
        assert!(right.x > center.x);
        let up = camera.project(Vec3::Y).expect("y in front");
        assert!(up.y < center.y);
        assert!(camera.project(Vec3::new(0.0, 0.0, 10.0)).is_none());
    }

    #[test]
    fn moving_a_spot_moves_its_aim_rigidly() {
        let camera = camera();
        let mut settings = light_settings(LightmapLightKind::Spot);
        settings.lights[0].position = [0.0; 3];
        settings.lights[0].target = [0.0, 0.0, -1.0];

        apply_light_drag(
            &mut settings,
            LightGizmoHandle::Body(0),
            egui::vec2(20.0, 0.0),
            &camera,
        );

        let light = &settings.lights[0];
        assert!(light.position[0] > 0.0);
        assert!((light.target[0] - light.position[0]).abs() < 1e-5);
        assert!((light.target[1] - light.position[1]).abs() < 1e-5);
        assert!((light.target[2] - light.position[2] + 1.0).abs() < 1e-5);
    }

    #[test]
    fn point_light_drag_leaves_the_unused_target_alone() {
        let camera = camera();
        let mut settings = light_settings(LightmapLightKind::Point);
        settings.lights[0].position = [0.0; 3];
        settings.lights[0].target = [2.0, 2.0, 2.0];

        apply_light_drag(
            &mut settings,
            LightGizmoHandle::Body(0),
            egui::vec2(20.0, 0.0),
            &camera,
        );

        assert!(settings.lights[0].position[0] > 0.0);
        assert_eq!(settings.lights[0].target, [2.0, 2.0, 2.0]);
    }

    #[test]
    fn aim_drag_rotates_without_moving_the_light_or_changing_the_distance() {
        let camera = camera();
        let mut settings = light_settings(LightmapLightKind::Area);
        settings.lights[0].position = [0.0; 3];
        settings.lights[0].target = [0.0, 0.0, -1.0];
        let distance = 1.0;

        apply_light_drag(
            &mut settings,
            LightGizmoHandle::Target(0),
            egui::vec2(20.0, 0.0),
            &camera,
        );

        let light = &settings.lights[0];
        assert_eq!(light.position, [0.0; 3]);
        assert!(light.target[0] > 0.0);
        let moved = Vec3::from_array(light.target).length();
        assert!(
            (moved - distance).abs() < 1e-4,
            "aim distance changed from {distance} to {moved}"
        );
    }

    #[test]
    fn the_aim_handle_sits_along_the_emission_axis() {
        let mut settings = light_settings(LightmapLightKind::Spot);
        settings.lights[0].position = [0.0, 0.0, 0.0];
        settings.lights[0].target = [0.0, 0.0, -2.0];

        let aim = aim_handle_point(&settings.lights[0]).expect("valid aim");
        assert!((aim - Vec3::new(0.0, 0.0, -AIM_LENGTH)).length() < 1e-5);
    }

    #[test]
    fn only_the_selected_light_exposes_a_rotation_ring() {
        let targets = [
            GizmoTarget {
                selection: LightSelection::Scene(0),
                handle: LightGizmoHandle::Body(0),
                screen: egui::pos2(100.0, 100.0),
                radius: ICON_RADIUS,
            },
            GizmoTarget {
                selection: LightSelection::Scene(1),
                handle: LightGizmoHandle::Body(1),
                screen: egui::pos2(300.0, 100.0),
                radius: ICON_RADIUS,
            },
        ];
        let on_ring = egui::pos2(100.0 + RING_RADIUS, 100.0);
        assert_eq!(
            pick_ring(&targets, on_ring, Some(LightSelection::Scene(0)))
                .expect("selected light ring")
                .handle,
            LightGizmoHandle::Target(0)
        );
        assert!(pick_ring(&targets, on_ring, Some(LightSelection::Scene(1))).is_none());
        assert!(pick_ring(&targets, on_ring, None).is_none());
    }

    #[test]
    fn light_gizmos_follow_the_lightmap_mode_and_toggle() {
        let mut state = EditorState::default();
        let mut runtime = LightmapRuntime::default();
        state.right_tab = RightTab::Lightmap;
        assert!(light_gizmos_visible(&state, &runtime));

        state.show_light_gizmos = false;
        assert!(!light_gizmos_visible(&state, &runtime));

        state.show_light_gizmos = true;
        state.right_tab = RightTab::Properties;
        assert!(!light_gizmos_visible(&state, &runtime));

        runtime.preview_enabled = true;
        assert!(light_gizmos_visible(&state, &runtime));
    }

    #[test]
    fn sun_drag_rotates_and_clamps() {
        let camera = camera();
        let mut settings = LightmapSettings::default();
        let azimuth = settings.sun_azimuth_deg;
        settings.sun_elevation_deg = 5.0;

        apply_light_drag(
            &mut settings,
            LightGizmoHandle::Sun,
            egui::vec2(20.0, -100.0),
            &camera,
        );
        assert!(settings.sun_azimuth_deg < azimuth);
        assert!(settings.sun_elevation_deg > 5.0);

        apply_light_drag(
            &mut settings,
            LightGizmoHandle::Sun,
            egui::vec2(0.0, 10_000.0),
            &camera,
        );
        assert_eq!(settings.sun_elevation_deg, 0.0);
    }

    #[test]
    fn off_screen_handles_clamp_inside_the_pane() {
        let camera = camera();
        let (screen, off_screen) =
            project_to_view(&camera, rect(), Vec3::new(3.0, 0.0, 0.0)).expect("point is in front");
        assert!(off_screen);
        assert!(rect().expand(1.0).contains(screen));

        let (center, off_screen) = project_to_view(&camera, rect(), Vec3::ZERO).expect("origin");
        assert!(!off_screen);
        assert!((center - rect().center()).length() < 1e-3);
    }

    #[test]
    fn nearest_icon_wins_the_hit_test() {
        let first = GizmoTarget {
            selection: LightSelection::Scene(0),
            handle: LightGizmoHandle::Body(0),
            screen: egui::pos2(100.0, 100.0),
            radius: 12.0,
        };
        let second = GizmoTarget {
            selection: LightSelection::Scene(1),
            handle: LightGizmoHandle::Body(1),
            screen: egui::pos2(110.0, 100.0),
            radius: 12.0,
        };
        assert_eq!(
            pick(&[first, second], egui::pos2(104.0, 100.0))
                .expect("inside both")
                .selection,
            LightSelection::Scene(0)
        );
        assert!(pick(&[first, second], egui::pos2(200.0, 200.0)).is_none());
    }
}
