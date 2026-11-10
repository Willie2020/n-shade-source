//! Touchpad, touch-screen and pen-tablet input shared by the viewports.
//!
//! egui already normalises wheel, precision-touchpad and pinch input into
//! `smooth_scroll_delta` and `zoom_delta`. What it cannot tell an application
//! is how those deltas should be used: a notched mouse wheel usually means
//! zoom, while a two-finger touchpad scroll usually means pan. This module
//! classifies one frame of input so both viewports behave the same way.

use super::*;

pub(crate) const MIN_CANVAS_ZOOM: f32 = 0.1;
pub(crate) const MAX_CANVAS_ZOOM: f32 = 8.0;

/// How much a notched wheel line changes the canvas zoom.
const WHEEL_ZOOM_SPEED: f32 = 0.0015;

/// One frame of scroll, zoom and touch input.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ScrollGesture {
    /// Smoothed scroll delta in screen points. Positive values move content
    /// right/down, matching natural touchpad scrolling.
    pub delta: egui::Vec2,
    /// True when a precision touchpad produced pixel deltas this frame.
    pub touchpad: bool,
    /// True when a notched wheel produced line/page deltas this frame.
    pub notched: bool,
    /// Pinch or Ctrl/Cmd+wheel zoom factor. `1.0` means no zoom.
    pub zoom: f32,
    /// Centre of an active multi-touch pinch.
    pub zoom_center: Option<egui::Pos2>,
    /// Two-finger translation on a touch screen.
    pub touch_pan: egui::Vec2,
}

impl ScrollGesture {
    pub(crate) fn has_scroll(self) -> bool {
        self.delta != egui::Vec2::ZERO
    }

    pub(crate) fn has_zoom(self) -> bool {
        self.zoom.is_finite() && (self.zoom - 1.0).abs() > 1e-4
    }

    /// Keeps the device hint sticky while egui smooths a notched wheel over
    /// several frames, so a wheel zoom is not mistaken for a touchpad pan.
    pub(crate) fn remember_device(self, state: &mut EditorState) {
        if self.touchpad {
            state.last_scroll_touchpad = true;
        } else if self.notched {
            state.last_scroll_touchpad = false;
        }
    }

    /// Zoom factor for a vertical scroll delta.
    pub(crate) fn wheel_zoom(self) -> f32 {
        (self.delta.y * WHEEL_ZOOM_SPEED).exp()
    }
}

/// Reads this frame's scroll and zoom input once.
pub(crate) fn scroll_gesture(ui: &egui::Ui) -> ScrollGesture {
    ui.input(|input| {
        let mut touchpad = false;
        let mut notched = false;
        for event in &input.events {
            if let egui::Event::MouseWheel { unit, delta, .. } = event {
                if *delta == egui::Vec2::ZERO {
                    continue;
                }
                match unit {
                    egui::MouseWheelUnit::Point => touchpad = true,
                    egui::MouseWheelUnit::Line | egui::MouseWheelUnit::Page => notched = true,
                }
            }
        }
        let multi = input.multi_touch();
        ScrollGesture {
            delta: input.smooth_scroll_delta,
            touchpad,
            notched,
            zoom: input.zoom_delta(),
            zoom_center: multi.map(|touch| touch.center_pos),
            touch_pan: multi.map_or(egui::Vec2::ZERO, |touch| touch.translation_delta),
        }
    })
}

/// Zooms the 2D canvas around `anchor`, keeping the document point under the
/// cursor or pinch centre fixed.
pub(crate) fn zoom_canvas_at(
    state: &mut EditorState,
    viewport_center: egui::Pos2,
    anchor: egui::Pos2,
    factor: f32,
) {
    if !factor.is_finite() || factor <= 0.0 {
        return;
    }
    let new_zoom = (state.zoom * factor).clamp(MIN_CANVAS_ZOOM, MAX_CANVAS_ZOOM);
    let actual = new_zoom / state.zoom;
    if (actual - 1.0).abs() < 1e-6 {
        return;
    }
    let image_center = viewport_center + state.pan;
    state.pan += (anchor - image_center) * (1.0 - actual);
    state.zoom = new_zoom;
}

/// Samples pen/tablet pressure for this frame.
///
/// Pressure is only live while a touch is in contact: hovering a stylus sends
/// zero-force updates on some drivers and would otherwise shrink the brush
/// ring before the tip touches the tablet. A mouse always reports full
/// pressure, so brush size does not change for non-pen users.
pub(crate) fn update_pen_pressure(ctx: &egui::Context, state: &mut EditorState) {
    // Windows Ink reaches the editor through the native pointer hook when it is
    // installed. That path is more reliable than translated touch events and
    // also reports hover, so it wins whenever a pen has been seen.
    if let Some((contact, pressure)) = crate::pen_input::sample() {
        let time = ctx.input(|input| input.time);
        apply_pen_sample(state, contact, pressure, time);
        return;
    }

    let (sample, touching, ended, time) = ctx.input(|input| {
        let mut sample = None;
        let mut ended = false;
        for event in &input.events {
            if let egui::Event::Touch { force, phase, .. } = event {
                match phase {
                    // A contact without force is a mouse or a plain touch
                    // screen. Clear any pen pressure from the previous stroke
                    // so the mouse does not inherit it.
                    egui::TouchPhase::Start if force.is_none() => ended = true,
                    egui::TouchPhase::End | egui::TouchPhase::Cancel => ended = true,
                    _ => {}
                }
                if matches!(phase, egui::TouchPhase::Start | egui::TouchPhase::Move) {
                    if let Some(force) = force {
                        sample = Some(*force);
                    }
                }
            }
        }
        (sample, input.any_touches(), ended, input.time)
    });

    if ended {
        state.pen_pressure_active = false;
        state.pen_pressure = 1.0;
    } else if let Some(sample) = sample {
        state.pen_detected = true;
        // `touching` is false for hover updates: those carry force 0 on some
        // drivers and must not shrink the brush ring before contact.
        if touching {
            let first_sample = !state.pen_pressure_active;
            state.pen_pressure_active = true;
            state.pen_pressure_time = time;
            let target = sample.clamp(0.0, 1.0);
            // Smooth the sample: digitizers jitter, and an unsmoothed value
            // makes the brush ring pulse while drawing. The first sample after
            // contact is taken as-is so the first dab is not too heavy.
            let blend = if first_sample { 1.0 } else { 0.55 };
            state.pen_pressure += (target - state.pen_pressure) * blend;
        }
    } else if !touching && time - state.pen_pressure_time > 0.15 {
        state.pen_pressure_active = false;
        state.pen_pressure = 1.0;
    }
}

/// Apply one native (Windows Ink) pen sample. Hover samples have `contact`
/// false and only keep the pen marked as detected.
fn apply_pen_sample(state: &mut EditorState, contact: bool, pressure: f32, time: f64) {
    state.pen_detected = true;
    if contact {
        let first_sample = !state.pen_pressure_active;
        state.pen_pressure_active = true;
        state.pen_pressure_time = time;
        let target = pressure.clamp(0.0, 1.0);
        let blend = if first_sample { 1.0 } else { 0.55 };
        state.pen_pressure += (target - state.pen_pressure) * blend;
    } else {
        state.pen_pressure_active = false;
        state.pen_pressure = 1.0;
    }
}

/// Icon row for the pen-pressure switches. The pen is the master switch;
/// size, flow and opacity are its child toggles. Each glyph carries its own
/// hover hint, which keeps the mask toolbar and the Settings window compact.
pub(crate) fn pen_toggle_row(ui: &mut egui::Ui, state: &mut EditorState) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        if icon_button(
            ui,
            Icon::Pen,
            state.pen_pressure_enabled,
            true,
            "Pen pressure\nUse stylus pressure to vary the brush while painting.",
        )
        .clicked()
        {
            state.pen_pressure_enabled = !state.pen_pressure_enabled;
        }

        // Child switches stay visible but disabled while the master is off;
        // their tooltip still explains what each one controls.
        let enabled = state.pen_pressure_enabled;
        let size = icon_button(
            ui,
            Icon::Stretch,
            state.pen_pressure_size,
            enabled,
            "Pressure controls size\nLight strokes stay at 20% of the configured radius.",
        );
        if enabled && size.clicked() {
            state.pen_pressure_size = !state.pen_pressure_size;
        }
        let flow = icon_button(
            ui,
            Icon::Brush,
            state.pen_pressure_flow,
            enabled,
            "Pressure controls flow\nLight strokes build up slowly without changing the dab footprint.",
        );
        if enabled && flow.clicked() {
            state.pen_pressure_flow = !state.pen_pressure_flow;
        }
        let opacity = icon_button(
            ui,
            Icon::Opacity,
            state.pen_pressure_opacity,
            enabled,
            "Pressure controls opacity\nScales each dab's opacity with stylus pressure.",
        );
        if enabled && opacity.clicked() {
            state.pen_pressure_opacity = !state.pen_pressure_opacity;
        }
    });
}

/// Compact pen-pressure controls used by the mask toolbar and the Paint
/// properties card.
pub(crate) fn pen_controls(ui: &mut egui::Ui, state: &mut EditorState) {
    pen_toggle_row(ui, state);
    ui.add_space(4.0);
    let (fraction, label, hint) = if state.pen_detected && state.pen_pressure_active {
        let percent = state.pen_pressure.clamp(0.0, 1.0) * 100.0;
        (
            percent / 100.0,
            format!("Live pressure · {percent:.0}%"),
            "Current stylus pressure. The curve below scales size, flow and opacity.",
        )
    } else if state.pen_detected {
        (
            0.0,
            "Pen detected · idle".to_string(),
            "The pen is detected but not touching the tablet.",
        )
    } else {
        (
            0.0,
            "No pen seen".to_string(),
            "No Windows Ink pressure is reaching the app. Enable Windows Ink in the \
             tablet driver, or use OpenTabletDriver's Windows Ink output plugin.",
        )
    };
    ui.add(
        egui::ProgressBar::new(fraction)
            .desired_width(ui.available_width().min(170.0))
            .text(egui::RichText::new(label).size(TEXT_SMALL)),
    )
    .on_hover_text(hint);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pressure_scales_size_and_flow() {
        let state = EditorState {
            pen_detected: true,
            pen_pressure_active: true,
            pen_pressure: 0.5,
            ..Default::default()
        };
        let radius = state.brush_radius_now();
        let flow = state.brush_flow_now();
        assert!(radius < state.brush_radius, "{radius}");
        assert!(radius > state.brush_radius * 0.5, "{radius}");
        assert!(flow < state.brush_flow, "{flow}");
        assert!(flow > state.brush_flow * 0.5, "{flow}");
    }

    #[test]
    fn mouse_reports_full_pressure() {
        let state = EditorState {
            pen_detected: true,
            pen_pressure_active: false,
            pen_pressure: 0.1,
            ..Default::default()
        };
        assert_eq!(state.brush_pressure(), 1.0);
        assert_eq!(state.brush_radius_now(), state.brush_radius);
        assert_eq!(state.brush_flow_now(), state.brush_flow);
    }

    #[test]
    fn pressure_toggles_disable_the_curve() {
        let state = EditorState {
            pen_detected: true,
            pen_pressure_active: true,
            pen_pressure: 0.25,
            pen_pressure_size: false,
            pen_pressure_flow: false,
            ..Default::default()
        };
        assert_eq!(state.brush_radius_now(), state.brush_radius);
        assert_eq!(state.brush_flow_now(), state.brush_flow);
    }

    fn gesture_for(event: egui::Event) -> ScrollGesture {
        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(event);
        let mut gesture = None;
        let output = ctx.run_ui(raw, |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                gesture = Some(scroll_gesture(ui));
            });
        });
        output.drop_without_applying_deltas();
        gesture.expect("central panel runs")
    }

    #[test]
    fn wheel_units_are_classified() {
        let touchpad = gesture_for(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(4.0, 4.0),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        });
        assert!(touchpad.touchpad);
        assert!(!touchpad.notched);

        let wheel = gesture_for(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: egui::vec2(0.0, 1.0),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        });
        assert!(wheel.notched);
        assert!(!wheel.touchpad);
    }

    #[test]
    fn touch_force_updates_pressure() {
        let ctx = egui::Context::default();
        let mut state = EditorState {
            pen_pressure: 0.4,
            ..Default::default()
        };
        let mut raw = egui::RawInput {
            time: Some(1.0),
            ..Default::default()
        };
        raw.events.push(egui::Event::Touch {
            device_id: egui::TouchDeviceId(1),
            id: egui::TouchId::from(1_u64),
            phase: egui::TouchPhase::Start,
            pos: egui::pos2(10.0, 10.0),
            force: Some(0.4),
        });
        let output = ctx.run_ui(raw, |ui| update_pen_pressure(ui.ctx(), &mut state));
        output.drop_without_applying_deltas();
        assert!(state.pen_detected);
        assert!(state.pen_pressure_active);
        assert!(
            (state.pen_pressure - 0.4).abs() < 1e-3,
            "{}",
            state.pen_pressure
        );
    }

    #[test]
    fn touch_end_releases_pressure() {
        let ctx = egui::Context::default();
        let mut state = EditorState {
            pen_detected: true,
            pen_pressure_active: true,
            pen_pressure: 0.4,
            ..Default::default()
        };
        let mut raw = egui::RawInput {
            time: Some(2.0),
            ..Default::default()
        };
        raw.events.push(egui::Event::Touch {
            device_id: egui::TouchDeviceId(1),
            id: egui::TouchId::from(1_u64),
            phase: egui::TouchPhase::End,
            pos: egui::pos2(10.0, 10.0),
            force: None,
        });
        let output = ctx.run_ui(raw, |ui| update_pen_pressure(ui.ctx(), &mut state));
        output.drop_without_applying_deltas();
        assert!(!state.pen_pressure_active);
        assert_eq!(state.pen_pressure, 1.0);
    }

    #[test]
    fn native_pen_samples_drive_pressure() {
        let mut state = EditorState::default();
        apply_pen_sample(&mut state, true, 0.4, 1.0);
        assert!(state.pen_detected);
        assert!(state.pen_pressure_active);
        assert!((state.pen_pressure - 0.4).abs() < 1e-6);

        apply_pen_sample(&mut state, false, 0.0, 1.1);
        assert!(!state.pen_pressure_active);
        assert_eq!(state.pen_pressure, 1.0);
        assert!(state.pen_detected, "hovering keeps the pen detected");
    }

    #[test]
    fn zoom_keeps_the_anchor_fixed() {
        let mut state = EditorState::default();
        let center = egui::pos2(100.0, 100.0);
        let anchor = egui::pos2(150.0, 100.0);
        let doc_offset = (anchor.x - center.x) / state.zoom;
        zoom_canvas_at(&mut state, center, anchor, 2.0);
        assert!((state.zoom - 2.0).abs() < 1e-6);
        let image_center = center + state.pan;
        let screen = image_center.x + doc_offset * state.zoom;
        assert!((screen - anchor.x).abs() < 1e-4, "{screen}");
    }
}
