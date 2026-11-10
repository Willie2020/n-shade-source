//! Group-wide shape and vector property editing.
//!
//! Selecting a group exposes the common bevel and vector settings of every
//! shape beneath it. Values that differ across the group are shown as mixed;
//! changing a control applies it to all descendant shapes at once. Selecting a
//! child layer still shows that child's individual properties.

use super::*;

#[derive(Clone, Copy, Default)]
struct AggF32 {
    first: f32,
    sum: f32,
    count: usize,
    mixed: bool,
}

impl AggF32 {
    fn push(&mut self, value: f32) {
        if self.count == 0 {
            self.first = value;
        } else if (value - self.first).abs() > 1e-4 {
            self.mixed = true;
        }
        self.sum += value;
        self.count += 1;
    }

    fn representative(&self) -> f32 {
        if self.mixed && self.count > 0 {
            self.sum / self.count as f32
        } else {
            self.first
        }
    }
}

#[derive(Clone, Copy, Default)]
struct AggBool {
    first: Option<bool>,
    mixed: bool,
}

impl AggBool {
    fn push(&mut self, value: bool) {
        match self.first {
            None => self.first = Some(value),
            Some(first) if first != value => self.mixed = true,
            _ => {}
        }
    }

    fn value(&self) -> bool {
        self.first.unwrap_or(false)
    }
}

#[derive(Clone, Copy, Default)]
struct RuleAgg {
    first: Option<FillRule>,
    mixed: bool,
    count: usize,
}

impl RuleAgg {
    fn push(&mut self, rule: FillRule) {
        match self.first {
            None => self.first = Some(rule),
            Some(first) if first != rule => self.mixed = true,
            _ => {}
        }
        self.count += 1;
    }

    fn common(&self) -> Option<FillRule> {
        if self.mixed {
            None
        } else {
            self.first
        }
    }
}

#[derive(Clone, Copy, Default)]
struct StyleAgg {
    first: Option<ShapeStyle>,
    mixed: bool,
}

impl StyleAgg {
    fn push(&mut self, style: ShapeStyle) {
        match self.first {
            None => self.first = Some(style),
            Some(first) if first != style => self.mixed = true,
            _ => {}
        }
    }

    fn common(&self) -> Option<ShapeStyle> {
        if self.mixed {
            None
        } else {
            self.first
        }
    }
}

#[derive(Default)]
struct ProfileAgg {
    first: Option<Profile>,
    mixed: bool,
}

impl ProfileAgg {
    fn push(&mut self, profile: &Profile) {
        match &self.first {
            None => self.first = Some(profile.clone()),
            Some(first) if first != profile => self.mixed = true,
            _ => {}
        }
    }

    fn common(&self) -> Option<&Profile> {
        if self.mixed {
            None
        } else {
            self.first.as_ref()
        }
    }
}

#[derive(Default)]
struct ShapeStats {
    shapes: usize,
    paths: usize,
    primitives: usize,
    radius: AggF32,
    /// Signed bevel depth: negative values are recessed shapes.
    depth: AggF32,
    softness: AggF32,
    smooth: AggF32,
    edge_aa: AggF32,
    stroke_width: AggF32,
    styles: StyleAgg,
    invert: AggBool,
    profiles: ProfileAgg,
    has_chamfer: bool,
    chamfer_angle: AggF32,
    fill_rules: RuleAgg,
}

fn collect_stats(layers: &[Layer], stats: &mut ShapeStats) {
    for layer in layers {
        match &layer.kind {
            LayerKind::Shape(shape) => {
                stats.shapes += 1;
                stats.radius.push(shape.radius_px);
                stats.depth.push(if shape.invert {
                    -shape.height_px
                } else {
                    shape.height_px
                });
                stats.softness.push(shape.softness_px);
                stats.smooth.push(shape.smooth_px);
                stats.edge_aa.push(shape.edge_aa_px);
                stats.stroke_width.push(shape.stroke_width_px);
                stats.styles.push(shape.style);
                stats.invert.push(shape.invert);
                stats.profiles.push(&shape.profile);
                if let Profile::Chamfer { degrees } = &shape.profile {
                    stats.has_chamfer = true;
                    stats.chamfer_angle.push(*degrees);
                }
                match &shape.source {
                    ShapeSource::Path(path) => {
                        stats.paths += 1;
                        stats.fill_rules.push(path.fill_rule);
                    }
                    ShapeSource::Primitive(_) => stats.primitives += 1,
                    ShapeSource::Raster(_) | ShapeSource::RasterRegion(_) => {}
                    ShapeSource::Text(_) => {}
                }
            }
            LayerKind::Group { children } => collect_stats(children, stats),
            _ => {}
        }
    }
}

/// Visits every shape below `layers`, touches the owning layer after the edit so
/// cached evaluations (which hash child revisions) are invalidated.
fn for_each_shape(layers: &mut [Layer], apply: &mut dyn FnMut(&mut ShapeLayer)) {
    for layer in layers {
        match &mut layer.kind {
            LayerKind::Shape(shape) => {
                apply(shape);
                layer.touch();
            }
            LayerKind::Group { children } => for_each_shape(children, apply),
            _ => {}
        }
    }
}

fn group_slider_row(
    ui: &mut egui::Ui,
    label: &str,
    aggregate: &AggF32,
    range: std::ops::RangeInclusive<f32>,
    speed: f32,
    decimals: usize,
) -> Option<f32> {
    let mut value = aggregate.representative();
    let mixed = aggregate.mixed;
    let mut changed = false;
    ui.horizontal(|ui| {
        field_label(ui, label);
        if mixed {
            ui.label(
                egui::RichText::new("mixed")
                    .size(TEXT_CAPTION)
                    .color(WARNING)
                    .italics(),
            );
        }
        let field_width = 52.0;
        // Use the live spacing rather than a hard-coded 6 px: the default is
        // 8 px here, and the extra 2 px per row made the resizable dock creep
        // wider every frame.
        let gap = ui.spacing().item_spacing.x;
        let slider_width = (ui.available_width() - field_width - gap).max(36.0);
        // The fixed-size M3 slider allocates exactly its width, so the row does
        // not feed a rounding overflow back into the panel.
        let (slider_changed, _) = m3_slider(ui, &mut value, range.clone(), slider_width);
        changed |= slider_changed;
        changed |= ui
            .add_sized(
                [field_width, 24.0],
                egui::DragValue::new(&mut value)
                    .range(range)
                    .clamp_existing_to_range(false)
                    .speed(speed)
                    .fixed_decimals(decimals),
            )
            .changed();
    });
    changed.then_some(value)
}

fn collect_shape_ids(layers: &[Layer], ids: &mut Vec<LayerId>) {
    for layer in layers {
        match &layer.kind {
            LayerKind::Shape(_) => ids.push(layer.id),
            LayerKind::Group { children } => collect_shape_ids(children, ids),
            _ => {}
        }
    }
}

/// Draws the group-wide shape editor. Returns whether a descendant changed and
/// an optional list of shape ids that should open the custom curve editor.
pub(crate) fn group_shape_properties(
    ui: &mut egui::Ui,
    children: &mut [Layer],
) -> (bool, Option<Vec<LayerId>>) {
    let mut stats = ShapeStats::default();
    collect_stats(children, &mut stats);

    ui.add_space(12.0);
    if stats.shapes == 0 {
        ui.label(
            egui::RichText::new(
                "No shape layers in this group. Add a shape or ungroup to edit vector properties.",
            )
            .size(TEXT_SMALL)
            .color(MUTED),
        );
        return (false, None);
    }

    let mut changed = false;
    let mut custom_request = None;
    let shape_pill = format!(
        "{} shape{}",
        stats.shapes,
        if stats.shapes == 1 { "" } else { "s" }
    );
    collapsible_section(
        ui,
        "group_shape_properties",
        Icon::Grid,
        "Group shape properties",
        Some((&shape_pill, true)),
        true,
        |ui| {
            let shape_word = if stats.shapes == 1 { "shape" } else { "shapes" };
            ui.label(
                egui::RichText::new(format!(
                    "Edits all {} {} below this group.",
                    stats.shapes, shape_word
                ))
                .size(TEXT_SMALL)
                .color(MUTED),
            );
            ui.label(
                egui::RichText::new(format!(
                    "{} path{} · {} primitive{} · {} raster",
                    stats.paths,
                    if stats.paths == 1 { "" } else { "s" },
                    stats.primitives,
                    if stats.primitives == 1 { "" } else { "s" },
                    stats
                        .shapes
                        .saturating_sub(stats.paths)
                        .saturating_sub(stats.primitives),
                ))
                .size(TEXT_CAPTION)
                .color(MUTED),
            );

            ui.horizontal(|ui| {
                field_label(ui, "Style");
                let current = stats.styles.common();
                let selected_text = match current {
                    Some(ShapeStyle::Fill) => "Fill",
                    Some(ShapeStyle::Stroke) => "Outline stroke",
                    None => "Mixed",
                };
                let mut choice = None;
                egui::ComboBox::from_id_salt("group_shape_style")
                    .width(ui.available_width())
                    .selected_text(selected_text)
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(current == Some(ShapeStyle::Fill), "Fill")
                            .clicked()
                        {
                            choice = Some(ShapeStyle::Fill);
                            ui.close();
                        }
                        if ui
                            .selectable_label(current == Some(ShapeStyle::Stroke), "Outline stroke")
                            .clicked()
                        {
                            choice = Some(ShapeStyle::Stroke);
                            ui.close();
                        }
                    });
                if let Some(style) = choice {
                    for_each_shape(children, &mut |shape| shape.style = style);
                    changed = true;
                }
            });
            if stats.styles.common() != Some(ShapeStyle::Fill) {
                if let Some(value) =
                    group_slider_row(ui, "Stroke (px)", &stats.stroke_width, 0.5..=256.0, 0.5, 1)
                {
                    for_each_shape(children, &mut |shape| shape.stroke_width_px = value);
                    changed = true;
                }
            }

            // Group radius stays tight: it applies to every shape below the group,
            // so a small range keeps the shared bevel readable and predictable.
            if let Some(value) =
                group_slider_row(ui, "Radius (px)", &stats.radius, 1.0..=10.0, 0.1, 1)
            {
                for_each_shape(children, &mut |shape| shape.radius_px = value);
                changed = true;
            }
            if let Some(value) =
                group_slider_row(ui, "Depth (px)", &stats.depth, -128.0..=128.0, 0.2, 1)
            {
                for_each_shape(children, &mut |shape| {
                    shape.height_px = value.abs();
                    shape.invert = value < 0.0;
                });
                changed = true;
            }
            if let Some(value) =
                group_slider_row(ui, "Softness (px)", &stats.softness, 0.0..=32.0, 0.1, 1)
            {
                for_each_shape(children, &mut |shape| shape.softness_px = value);
                changed = true;
            }
            if let Some(value) =
                group_slider_row(ui, "Smooth (px)", &stats.smooth, 0.0..=8.0, 0.1, 1)
            {
                for_each_shape(children, &mut |shape| shape.smooth_px = value);
                changed = true;
            }
            if let Some(value) =
                group_slider_row(ui, "Anti-alias", &stats.edge_aa, 0.0..=8.0, 0.1, 1)
            {
                for_each_shape(children, &mut |shape| shape.edge_aa_px = value);
                changed = true;
            }

            ui.add_space(2.0);
            let mut invert = stats.invert.value();
            ui.horizontal(|ui| {
                if toggle_switch(ui, &mut invert).changed() {
                    for_each_shape(children, &mut |shape| shape.invert = invert);
                    changed = true;
                }
                ui.label(egui::RichText::new("Recessed (carve)").size(12.0));
                if stats.invert.mixed {
                    ui.label(
                        egui::RichText::new("mixed")
                            .size(TEXT_CAPTION)
                            .color(WARNING)
                            .italics(),
                    );
                }
            });

            ui.add_space(2.0);
            ui.horizontal(|ui| {
                field_label(ui, "Profile");
                let current = stats.profiles.common().cloned();
                let selected_text = current
                    .as_ref()
                    .map(profile_label)
                    .unwrap_or_else(|| "Mixed".into());
                let mut choice = None;
                egui::ComboBox::from_id_salt("group_profile")
                    .width(ui.available_width())
                    .height(260.0)
                    .selected_text(selected_text)
                    .show_ui(ui, |ui| {
                        for (profile, label) in profile_choices() {
                            if ui
                                .selectable_label(current.as_ref() == Some(&profile), label)
                                .clicked()
                            {
                                choice = Some(profile);
                                ui.close();
                            }
                        }
                        ui.separator();
                        if ui
                            .selectable_label(
                                matches!(current, Some(Profile::Custom(_))),
                                "Custom curve...",
                            )
                            .clicked()
                        {
                            let mut ids = Vec::new();
                            collect_shape_ids(children, &mut ids);
                            custom_request = Some(ids);
                            ui.close();
                        }
                    });
                if let Some(profile) = choice {
                    for_each_shape(children, &mut |shape| {
                        shape.profile = profile.clone();
                    });
                    changed = true;
                }
            });

            if stats.has_chamfer {
                if let Some(value) = group_slider_row(
                    ui,
                    "Chamfer angle",
                    &stats.chamfer_angle,
                    5.0..=85.0,
                    0.5,
                    1,
                ) {
                    for_each_shape(children, &mut |shape| {
                        if let Profile::Chamfer { degrees } = &mut shape.profile {
                            *degrees = value;
                        }
                    });
                    changed = true;
                }
            }

            if stats.paths > 0 {
                ui.horizontal(|ui| {
                    field_label(ui, "Fill rule");
                    let current = stats.fill_rules.common();
                    let selected_text = match current {
                        Some(FillRule::NonZero) => "Non-zero",
                        Some(FillRule::EvenOdd) => "Even-odd",
                        None => "Mixed",
                    };
                    let mut choice = None;
                    egui::ComboBox::from_id_salt("group_fill_rule")
                        .width(ui.available_width())
                        .selected_text(selected_text)
                        .show_ui(ui, |ui| {
                            if ui
                                .selectable_label(current == Some(FillRule::NonZero), "Non-zero")
                                .clicked()
                            {
                                choice = Some(FillRule::NonZero);
                                ui.close();
                            }
                            if ui
                                .selectable_label(current == Some(FillRule::EvenOdd), "Even-odd")
                                .clicked()
                            {
                                choice = Some(FillRule::EvenOdd);
                                ui.close();
                            }
                        });
                    if let Some(rule) = choice {
                        for_each_shape(children, &mut |shape| {
                            if let ShapeSource::Path(path) = &mut shape.source {
                                path.fill_rule = rule;
                            }
                        });
                        changed = true;
                    }
                });
            }

            if stats.primitives > 0 {
                ui.add_space(2.0);
                let label = format!(
                    "Convert {} primitive{} to editable paths",
                    stats.primitives,
                    if stats.primitives == 1 { "" } else { "s" }
                );
                if ui.button(label).clicked() {
                    for_each_shape(children, &mut |shape| {
                        if matches!(&shape.source, ShapeSource::Primitive(_)) {
                            if let Some(path) = shape.source.to_path() {
                                shape.source = ShapeSource::Path(path);
                            }
                        }
                    });
                    changed = true;
                }
            }

            ui.add_space(3.0);
            ui.label(
                egui::RichText::new(
                    "Select a child on the canvas or in the Layers list to edit its own path \
                 nodes and geometry individually.",
                )
                .size(TEXT_CAPTION)
                .color(MUTED),
            );
        },
    );

    (changed, custom_request)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(radius: f32, invert: bool) -> Layer {
        let mut layer = Layer::shape(
            "shape",
            ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::ZERO,
                size: Vec2::splat(10.0),
                corner_radius: 2.0,
            }),
        );
        if let LayerKind::Shape(shape) = &mut layer.kind {
            shape.radius_px = radius;
            shape.invert = invert;
        }
        layer
    }

    #[test]
    fn stats_detect_mixed_values_across_nested_groups() {
        let layers = vec![
            shape(10.0, false),
            Layer::group("group", vec![shape(20.0, true), Layer::flat("base")]),
        ];
        let mut stats = ShapeStats::default();
        collect_stats(&layers, &mut stats);
        assert_eq!(stats.shapes, 2);
        assert!(stats.radius.mixed);
        assert!(stats.invert.mixed);
        assert!(stats.profiles.common().is_some());
    }

    #[test]
    fn for_each_shape_updates_nested_shapes_and_bumps_revisions() {
        let mut layers = vec![
            shape(10.0, false),
            Layer::group("group", vec![shape(20.0, false)]),
        ];
        for_each_shape(&mut layers, &mut |shape| shape.radius_px = 42.0);
        let mut radii = Vec::new();
        let mut revisions = Vec::new();
        fn walk(layers: &[Layer], radii: &mut Vec<f32>, revisions: &mut Vec<u64>) {
            for layer in layers {
                match &layer.kind {
                    LayerKind::Shape(shape) => {
                        radii.push(shape.radius_px);
                        revisions.push(layer.revision);
                    }
                    LayerKind::Group { children } => walk(children, radii, revisions),
                    _ => {}
                }
            }
        }
        walk(&layers, &mut radii, &mut revisions);
        assert_eq!(radii, vec![42.0, 42.0]);
        assert!(revisions.iter().all(|revision| *revision > 0));
    }

    #[test]
    fn group_slider_row_fits_the_available_width() {
        let ctx = egui::Context::default();
        let mut aggregate = AggF32::default();
        // Mixed values exercise the extra "mixed" chip in the row.
        aggregate.push(4.0);
        aggregate.push(8.0);
        let mut row_width = 0.0;
        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.set_width(276.0);
            let row = ui.scope(|ui| {
                group_slider_row(ui, "Radius (px)", &aggregate, 1.0..=10.0, 0.1, 1);
            });
            row_width = row.response.rect.width();
        });
        output.drop_without_applying_deltas();
        assert!(
            row_width <= 276.01,
            "group slider row overflowed its {:.0} px slot: {row_width:.2} px",
            276.0
        );
    }
}
