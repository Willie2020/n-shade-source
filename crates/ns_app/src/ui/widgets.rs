//! Reusable egui widgets shared by the panels.

use super::*;
pub(crate) fn toggle_switch(ui: &mut egui::Ui, value: &mut bool) -> egui::Response {
    // Reference switch: 34 x 20 track, 14 px white knob inset 3 px.
    let desired = egui::vec2(34.0, 20.0);
    let (rect, mut response) = ui.allocate_exact_size(desired, egui::Sense::click());
    if response.clicked() {
        *value = !*value;
        response.mark_changed();
    }
    let progress = ui.ctx().animate_bool(response.id, *value);
    let track = if *value {
        PRIMARY
    } else {
        INSPECTOR_LINE_STRONG
    };
    ui.painter()
        .rect_filled(rect, egui::CornerRadius::same(10), track);
    if response.hovered() {
        ui.painter().rect_filled(
            rect,
            egui::CornerRadius::same(10),
            state_layer(track, ON_SURFACE, HOVER_ALPHA),
        );
    }
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect.expand(2.0),
            egui::CornerRadius::same(12),
            egui::Stroke::new(2.0_f32, PRIMARY),
            egui::StrokeKind::Outside,
        );
    }
    let knob_x = egui::lerp(rect.left() + 10.0..=rect.right() - 10.0, progress);
    ui.painter().circle_filled(
        egui::pos2(knob_x, rect.center().y),
        7.0,
        egui::Color32::WHITE,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Tool-rail cell metrics from the reference (guide §14): 56 wide and
/// 51.4 tall = 8 top + 18 icon + 4 gap + 15.4 label line + 6 bottom.
pub(crate) const TOOL_CELL_WIDTH: f32 = 56.0;
pub(crate) const TOOL_CELL_HEIGHT: f32 = 51.4;
/// Radius of the cell background.
const TOOL_CELL_RADIUS: u8 = 10;
/// Cell-top to icon centre (8 px padding + half the 18 px glyph).
const TOOL_ICON_CENTRE: f32 = 8.0 + 9.0;
/// Cell-top to label centre (icon + gap + half the 11 px x 1.4 line box).
const TOOL_LABEL_CENTRE: f32 = 8.0 + 18.0 + 4.0 + 15.4 / 2.0;

pub(crate) fn rail_tool(
    ui: &mut egui::Ui,
    active: bool,
    glyph: &str,
    label: &str,
    tooltip: &str,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(TOOL_CELL_WIDTH, TOOL_CELL_HEIGHT),
        egui::Sense::click(),
    );
    let (fill, ink) = if active {
        (ACCENT_WASH, PRIMARY)
    } else if response.hovered() {
        (CONTAINER, ON_SURFACE)
    } else {
        (egui::Color32::TRANSPARENT, ON_SURFACE_VAR)
    };
    ui.painter()
        .rect_filled(rect, egui::CornerRadius::same(TOOL_CELL_RADIUS), fill);
    let _ = paint_icon(
        ui.painter(),
        glyph,
        egui::pos2(rect.center().x, rect.top() + TOOL_ICON_CENTRE),
        18.0,
        ink,
    );
    ui.painter().text(
        egui::pos2(rect.center().x, rect.top() + TOOL_LABEL_CENTRE),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(11.0),
        ink,
    );
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(tooltip)
}

pub(crate) fn rail_flyout_marker(ui: &mut egui::Ui, rect: egui::Rect, active: bool) {
    // Tucked against the icon's lower-right corner, not the tile edge, so the
    // marker reads as part of the tool rather than a stray dot in the rail.
    let color = if active { ACCENT_TEXT } else { MUTED };
    let p = egui::pos2(rect.center().x + 11.0, rect.top() + 27.0);
    ui.painter().add(egui::Shape::convex_polygon(
        vec![p, p + egui::vec2(-4.0, 0.0), p + egui::vec2(0.0, -4.0)],
        color,
        egui::Stroke::NONE,
    ));
}

pub(crate) fn tab_button(ui: &mut egui::Ui, label: &str, selected: bool) -> egui::Response {
    // Two-line labels, as in the reference: "3D View" stacks, "Split" does not.
    let text = label.replace(' ', "\n");
    let font = egui::FontId::proportional(14.0);
    let text_color = if selected { ON_SURFACE } else { ON_SURFACE_VAR };
    let galley = ui.painter().layout_no_wrap(text, font, text_color);
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(galley.size().x + 28.0, 44.0),
        egui::Sense::click(),
    );
    let radius = egui::CornerRadius::same(RADIUS_SM);
    if selected {
        ui.painter().rect_filled(rect, radius, CONTAINER);
        // Accent rule tucked inside the bottom edge of the chip.
        ui.painter().rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(rect.left(), rect.bottom() - 2.0),
                rect.right_bottom(),
            ),
            egui::CornerRadius {
                nw: 0,
                ne: 0,
                sw: 2,
                se: 2,
            },
            PRIMARY,
        );
    } else if response.hovered() {
        ui.painter()
            .rect_filled(rect, radius, hover_layer(CONTAINER_LOW));
    }
    let pos = rect.center() - galley.size() * 0.5;
    ui.painter().galley(pos, galley.clone(), text_color);
    if selected {
        // egui's bundled face has no bold, so the active tab is drawn a hair
        // wider; the reference tabs read noticeably heavier than the rest.
        ui.painter()
            .galley(pos + egui::vec2(0.4, 0.0), galley, text_color);
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

// ---------------------------------------------------------------------------
// Viewport bar (guide §14)
// ---------------------------------------------------------------------------

/// Height of the viewport bar, separator included (CSS `height:60px`, border-box).
pub(crate) const VIEWPORT_BAR_HEIGHT: f32 = 60.0;

/// CSS `box-shadow: inset <x> <y> 0 <colour>` on a rounded box.
///
/// The shadow is the padding box minus the same box shifted by `offset`, so for
/// an axis-aligned offset it is itself a rounded rect (same radius) with two
/// edges moved. Painting the box in the bar colour and then that smaller rect in
/// the background colour leaves the exact crescent, corners included.
///
/// `inset 3px 0 0` → offset `(3, 0)` (left bar); `inset 0 -2px 0` → `(0, -2)`
/// (bottom bar).
pub(crate) fn inset_bar(
    painter: &egui::Painter,
    rect: egui::Rect,
    radius: u8,
    offset: egui::Vec2,
    bar: egui::Color32,
    background: egui::Color32,
) {
    painter.rect_filled(rect, egui::CornerRadius::same(radius), bar);
    painter.rect_filled(
        inset_rect(rect, offset),
        egui::CornerRadius::same(radius),
        background,
    );
}

/// The padding box a CSS inset shadow leaves behind: the box with two edges
/// pulled in by `offset`.
pub(crate) fn inset_rect(rect: egui::Rect, offset: egui::Vec2) -> egui::Rect {
    egui::Rect::from_min_max(
        rect.min + egui::vec2(offset.x.max(0.0), offset.y.max(0.0)),
        rect.max + egui::vec2(offset.x.min(0.0), offset.y.min(0.0)),
    )
}

/// `.vtab`: padding 10 x 18, radius 6, line-height 1.15.
///
/// Selected tabs use SURFACE, the semibold face and a 2 px accent rule inset
/// along the bottom edge. The weight change makes the selected tab wider, as the
/// CSS layout reflows around it; keep it, a fixed width is a visible mismatch.
pub(crate) fn bar_tab(ui: &mut egui::Ui, label: &str, selected: bool) -> egui::Response {
    let font = if selected {
        bold(14.0)
    } else {
        egui::FontId::proportional(14.0)
    };
    let line_height = 14.0 * 1.15;
    let lines: Vec<&str> = label.split('\n').collect();
    let text_width = lines
        .iter()
        .map(|line| {
            ui.painter()
                .layout_no_wrap((*line).to_owned(), font.clone(), egui::Color32::WHITE)
                .size()
                .x
        })
        .fold(0.0_f32, f32::max);
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(text_width + 36.0, line_height * lines.len() as f32 + 20.0),
        egui::Sense::click(),
    );
    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, egui::CornerRadius::same(RADIUS_SM), CONTAINER);
        inset_bar(
            painter,
            rect,
            RADIUS_SM,
            egui::vec2(0.0, -2.0),
            PRIMARY,
            CONTAINER,
        );
    } else if response.hovered() {
        painter.rect_filled(rect, egui::CornerRadius::same(RADIUS_SM), CONTAINER);
    }
    let ink = if selected || response.hovered() {
        ON_SURFACE
    } else {
        ON_SURFACE_VAR
    };
    for (index, line) in lines.iter().enumerate() {
        painter.text(
            egui::pos2(
                rect.center().x,
                rect.top() + 10.0 + line_height * (index as f32 + 0.5),
            ),
            egui::Align2::CENTER_CENTER,
            *line,
            font.clone(),
            ink,
        );
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Shared body of the `.ib` buttons: 34 x 34, radius 6, 18 px glyph.
fn bar_button(
    ui: &mut egui::Ui,
    pressed: bool,
    enabled: bool,
    tooltip: &str,
    paint_glyph: impl FnOnce(&egui::Painter, egui::Pos2, f32, egui::Color32),
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::Vec2::splat(34.0), egui::Sense::click());
    let hovered = enabled && response.hovered();
    let (fill, ink) = if !enabled {
        (egui::Color32::TRANSPARENT, TEXT_DISABLED)
    } else if pressed {
        (ACCENT_WASH, PRIMARY)
    } else if hovered {
        (CONTAINER, ON_SURFACE)
    } else {
        (egui::Color32::TRANSPARENT, ON_SURFACE_VAR)
    };
    ui.painter()
        .rect_filled(rect, egui::CornerRadius::same(RADIUS_SM), fill);
    paint_glyph(ui.painter(), rect.center(), 18.0, ink);
    let response = response.on_hover_cursor(if enabled {
        egui::CursorIcon::PointingHand
    } else {
        egui::CursorIcon::Default
    });
    if enabled {
        response.on_hover_text(tooltip)
    } else {
        response.on_disabled_hover_text(tooltip)
    }
}

/// `.ib` with a sprite icon: idle `--muted`, hover SURFACE + `--text`,
/// pressed accent-soft + accent.
pub(crate) fn bar_icon_button(
    ui: &mut egui::Ui,
    name: &str,
    pressed: bool,
    enabled: bool,
    tooltip: &str,
) -> egui::Response {
    bar_button(
        ui,
        pressed,
        enabled,
        tooltip,
        |painter, center, size, ink| {
            let _ = paint_icon(painter, name, center, size, ink);
        },
    )
}

/// `.ib` with one of the editor's hand-drawn glyphs for controls the sprite
/// does not cover (split orientation, mask/UV overlays, auto-rotate).
pub(crate) fn bar_glyph_button(
    ui: &mut egui::Ui,
    icon: Icon,
    pressed: bool,
    enabled: bool,
    tooltip: &str,
) -> egui::Response {
    bar_button(
        ui,
        pressed,
        enabled,
        tooltip,
        move |painter, center, size, ink| {
            draw_icon(
                painter,
                egui::Rect::from_center_size(center, egui::Vec2::splat(size)),
                icon,
                ink,
            );
        },
    )
}

/// `.sel select`: SURFACE, 1 px LINE, radius 6, padding 14 x 9 and a 16 px
/// chevron 12 px from the right edge, supplied through `ComboBox::icon`.
/// `width` is the CSS border-box width.
pub(crate) fn bar_select(
    ui: &mut egui::Ui,
    id: &str,
    selected_text: &str,
    width: f32,
    menu: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    ui.scope(|ui| {
        ui.spacing_mut().button_padding = egui::vec2(14.0, 9.0);
        ui.spacing_mut().icon_width = 16.0;
        egui::ComboBox::from_id_salt(id)
            .width(width)
            .selected_text(
                egui::RichText::new(selected_text)
                    .size(14.0)
                    .color(ON_SURFACE),
            )
            .selected_text(
                egui::RichText::new(selected_text)
                    .size(14.0)
                    .color(ON_SURFACE),
            )
            .icon(|ui, rect, _visuals, _open| {
                let _ = paint_icon(
                    ui.painter(),
                    "down",
                    rect.center() + egui::vec2(2.0, 0.0),
                    16.0,
                    ON_SURFACE_VAR,
                );
            })
            .show_ui(ui, menu)
    })
    .response
}

/// Mix two colours in sRGB, as CSS gradients do.
pub(crate) fn lerp_color(a: egui::Color32, b: egui::Color32, t: f32) -> egui::Color32 {
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t.clamp(0.0, 1.0)).round() as u8;
    egui::Color32::from_rgb(mix(a.r(), b.r()), mix(a.g(), b.g()), mix(a.b(), b.b()))
}

/// Rounded rect filled with a two-stop linear gradient.
///
/// egui has no gradient brush, so the shape is a triangle fan over the rounded
/// outline with a per-vertex colour: exact for a linear ramp. `Shape::mesh` is
/// not anti-aliased, so the outline is a core ring inset 0.5 px (opaque) plus a
/// fringe ring outset 0.5 px (alpha 0): 50 % coverage lands on the edge.
pub(crate) fn fill_rounded_gradient(
    painter: &egui::Painter,
    rect: egui::Rect,
    radius: f32,
    from: egui::Color32,
    to: egui::Color32,
    axis: egui::Vec2,
) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let axis = axis.normalized();
    let center = rect.center();
    let half = (axis.x.abs() * rect.width() + axis.y.abs() * rect.height()) * 0.5;
    let color_at = |p: egui::Pos2| {
        let t = ((p - center).dot(axis) / (half * 2.0)) + 0.5;
        lerp_color(from, to, t)
    };
    let r = radius
        .min(rect.width() * 0.5)
        .min(rect.height() * 0.5)
        .max(0.0);
    let segments = 12;
    let mut ring: Vec<(egui::Pos2, egui::Vec2)> = Vec::with_capacity((segments + 1) * 4);
    let corners = [
        (
            rect.right() - r,
            rect.top() + r,
            -std::f32::consts::FRAC_PI_2,
        ),
        (rect.right() - r, rect.bottom() - r, 0.0),
        (
            rect.left() + r,
            rect.bottom() - r,
            std::f32::consts::FRAC_PI_2,
        ),
        (rect.left() + r, rect.top() + r, std::f32::consts::PI),
    ];
    for (cx, cy, start) in corners {
        for step in 0..=segments {
            let angle = start + std::f32::consts::FRAC_PI_2 * step as f32 / segments as f32;
            let normal = egui::vec2(angle.cos(), angle.sin());
            ring.push((egui::pos2(cx, cy) + normal * r, normal));
        }
    }

    let count = ring.len() as u32;
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(center, color_at(center));
    for (point, normal) in &ring {
        mesh.colored_vertex(*point - *normal * 0.5, color_at(*point));
    }
    for (point, normal) in &ring {
        mesh.colored_vertex(*point + *normal * 0.5, egui::Color32::TRANSPARENT);
    }
    for index in 0..count {
        let next = (index + 1) % count;
        mesh.add_triangle(0, 1 + index, 1 + next);
        mesh.add_triangle(1 + index, 1 + count + index, 1 + count + next);
        mesh.add_triangle(1 + index, 1 + count + next, 1 + next);
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// State pill for a section heading (`pill` in the reference): a compact,
/// fully rounded chip that reports a mode ("Balanced") or a count ("2
/// active"). Active pills use the accent wash; inactive ones stay quiet on the
/// card surface so they do not compete with the title.
pub(crate) fn pill(ui: &mut egui::Ui, text: &str, on: bool) -> egui::Response {
    let font = egui::FontId::proportional(11.5);
    let ink = if on { PRIMARY } else { INSPECTOR_MUTED };
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, ink);
    let size = egui::vec2(galley.size().x + 16.0, (galley.size().y + 6.0).max(20.0));
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
    let radius = egui::CornerRadius::same((size.y * 0.5).ceil() as u8);
    let (fill, stroke) = if on {
        (
            ACCENT_WASH,
            egui::Stroke::new(1.0_f32, PRIMARY.gamma_multiply(0.35)),
        )
    } else {
        (
            egui::Color32::from_white_alpha(10),
            egui::Stroke::new(1.0_f32, INSPECTOR_LINE),
        )
    };
    ui.painter()
        .rect(rect, radius, fill, stroke, egui::StrokeKind::Inside);
    ui.painter()
        .galley(rect.center() - galley.size() * 0.5, galley, ink);
    response
}

/// Ghost button on the darkest step (`.wide` in the reference): mask and clip
/// actions that should not compete with the accent.
pub(crate) fn ghost_button(label: impl Into<egui::WidgetText>) -> egui::Button<'static> {
    egui::Button::new(label.into())
        .fill(SURFACE_LOWEST)
        .stroke(egui::Stroke::NONE)
        .corner_radius(egui::CornerRadius::same(RADIUS_SM))
}

/// Accent-tinted text button (`.btn` in the reference): the secondary action of
/// a panel, e.g. `+ Add`.
pub(crate) fn accent_button(label: impl Into<egui::WidgetText>) -> egui::Button<'static> {
    egui::Button::new(label.into())
        .fill(ACCENT_WASH)
        .stroke(egui::Stroke::new(1.0_f32, PRIMARY.gamma_multiply(0.35)))
        .corner_radius(egui::CornerRadius::same(RADIUS_SM))
}

/// Disclosure triangle drawn as a shape (the default fonts lack arrow glyphs).
pub(crate) fn chevron_button(ui: &mut egui::Ui, open: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(rect, 4.0, HOVER_BG);
    }
    let c = rect.center();
    let s = 4.5;
    let points = if open {
        vec![
            c + egui::vec2(-s, -s * 0.5),
            c + egui::vec2(s, -s * 0.5),
            c + egui::vec2(0.0, s * 0.7),
        ]
    } else {
        vec![
            c + egui::vec2(-s * 0.5, -s),
            c + egui::vec2(s * 0.7, 0.0),
            c + egui::vec2(-s * 0.5, s),
        ]
    };
    let color = if response.hovered() { TEXT } else { MUTED };
    ui.painter().add(egui::Shape::convex_polygon(
        points,
        color,
        egui::Stroke::NONE,
    ));
    response
}

pub(crate) fn status_separator(ui: &mut egui::Ui) {
    ui.label(egui::RichText::new("·").size(TEXT_LABEL).color(DIVIDER));
}
/// Fixed-width label column so rows line up. Labels are left-aligned and
/// truncated (full text on hover) instead of wrapping into the next row.
pub(crate) fn field_label(ui: &mut egui::Ui, text: &str) {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(FIELD_LABEL_WIDTH, 24.0), egui::Sense::hover());
    let font = egui::FontId::proportional(TEXT_SMALL);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font.clone(), INSPECTOR_MUTED);
    if galley.size().x <= rect.width() {
        ui.painter().galley(
            egui::pos2(rect.left(), rect.center().y - galley.size().y * 0.5),
            galley,
            INSPECTOR_MUTED,
        );
    } else {
        let mut job =
            egui::text::LayoutJob::simple_singleline(text.to_owned(), font, INSPECTOR_MUTED);
        job.wrap = egui::text::TextWrapping::truncate_at_width(rect.width());
        let galley = ui.painter().layout_job(job);
        ui.painter().galley(
            egui::pos2(rect.left(), rect.center().y - galley.size().y * 0.5),
            galley,
            INSPECTOR_MUTED,
        );
        response.on_hover_text(text);
    }
}

pub(crate) const FIELD_LABEL_WIDTH: f32 = 84.0;

/// Expandable section: a flat header row with a cyan icon, an optional state
/// pill, a chevron and a hairline above.
///
/// This mirrors the reference's `sec-h` instead of egui's default collapser:
/// the icon takes the triangle's place, the title carries the reading weight,
/// and the pill reports state without adding another row.
#[allow(clippy::too_many_arguments)]
pub(crate) fn collapsible_section<R>(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    icon: Icon,
    title: &str,
    pill_state: Option<(&str, bool)>,
    default_open: bool,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> Option<egui::InnerResponse<R>> {
    let id = ui.make_persistent_id(id_salt);
    let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(
        ui.ctx(),
        id,
        default_open,
    );
    let divider_index = ui.painter().add(egui::Shape::Noop);
    let content = ui
        .horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let (rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
            draw_icon(ui.painter(), rect, icon, PRIMARY);
            ui.add_space(4.0);
            // Reserve the right edge for the chevron and the pill before the
            // title takes its width, so a long heading truncates instead of
            // pushing them outside the card on a narrow dock.
            let pill_width = pill_state.map_or(0.0, |(text, _)| {
                ui.painter()
                    .layout_no_wrap(
                        text.to_owned(),
                        egui::FontId::proportional(11.5),
                        INSPECTOR_MUTED,
                    )
                    .size()
                    .x
                    + 18.0
            });
            let title_width = (ui.available_width() - 20.0 - pill_width).max(36.0);
            // `add_sized` centers its child; allocate a left-aligned slot so the
            // title stays next to the icon while still truncating on a narrow
            // dock.
            ui.allocate_ui_with_layout(
                egui::vec2(title_width, 18.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(title)
                                .font(bold(13.5))
                                .color(INSPECTOR_TEXT),
                        )
                        .truncate(),
                    );
                },
            );
            if let Some((text, on)) = pill_state {
                ui.add_space(2.0);
                pill(ui, text, on);
            }
        })
        .response;
    paint_section_chevron(
        ui.painter(),
        egui::pos2(ui.max_rect().right() - 7.0, content.rect.center().y),
        state.is_open(),
    );
    // The reference header is a full-width button. `horizontal` only reports
    // the content width, so extend the hit area and hover wash to the card edge.
    let band = egui::Rect::from_min_max(
        egui::pos2(ui.max_rect().left(), content.rect.top() - 2.0),
        egui::pos2(ui.max_rect().right(), content.rect.bottom() + 2.0),
    );
    let header = ui
        .interact(band, id.with("header"), egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);

    if header.hovered() {
        ui.painter().set(
            divider_index,
            egui::Shape::rect_filled(
                header.rect.expand2(egui::vec2(4.0, 2.0)),
                egui::CornerRadius::same(RADIUS_XS),
                hover_layer(CONTAINER),
            ),
        );
    } else {
        let rule_y = header.rect.top() - 5.0;
        ui.painter().set(
            divider_index,
            egui::Shape::line_segment(
                [
                    egui::pos2(ui.max_rect().left(), rule_y),
                    egui::pos2(ui.max_rect().right(), rule_y),
                ],
                egui::Stroke::new(1.0_f32, OUTLINE_VARIANT),
            ),
        );
    }
    if header.clicked() {
        state.toggle(ui);
    }
    state.show_body_unindented(ui, add_contents)
}

/// Down chevron when expanded, right chevron when collapsed (`sec-h .chev`).
fn paint_section_chevron(painter: &egui::Painter, center: egui::Pos2, open: bool) {
    let points = if open {
        vec![
            center + egui::vec2(-4.0, -2.0),
            center + egui::vec2(0.0, 2.0),
            center + egui::vec2(4.0, -2.0),
        ]
    } else {
        vec![
            center + egui::vec2(-2.0, -4.0),
            center + egui::vec2(2.0, 0.0),
            center + egui::vec2(-2.0, 4.0),
        ]
    };
    painter.add(egui::Shape::line(
        points,
        egui::Stroke::new(1.6_f32, INSPECTOR_DIM),
    ));
}

pub(crate) fn slider_row(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    speed: f32,
    decimals: usize,
) -> bool {
    slider_row_impl(ui, label, value, range, speed, decimals, true)
}

/// Same row, but values already outside `range` stay readable and unchanged;
/// only an edit clamps them. Used where the range is deliberately narrower
/// than values that may exist in older projects.
pub(crate) fn slider_row_soft_range(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    speed: f32,
    decimals: usize,
) -> bool {
    slider_row_impl(ui, label, value, range, speed, decimals, false)
}

/// Width the drag field needs for the widest value it can show. A fixed field
/// width made high-precision values (Cage % with four decimals) overflow the
/// row, which egui's resizable side panel then adopted as its new width.
fn drag_field_width(ui: &egui::Ui, range: &std::ops::RangeInclusive<f32>, decimals: usize) -> f32 {
    let widest = [*range.start(), *range.end()]
        .into_iter()
        .map(|value| format!("{value:.decimals$}"))
        .max_by_key(|text| text.len())
        .unwrap_or_default();
    let text_style = ui.style().drag_value_text_style.clone();
    let font = ui
        .style()
        .text_styles
        .get(&text_style)
        .cloned()
        .unwrap_or_else(|| egui::FontId::proportional(13.0));
    let text_width = ui
        .painter()
        .layout_no_wrap(widest, font, egui::Color32::PLACEHOLDER)
        .size()
        .x;
    (text_width + 2.0 * ui.spacing().button_padding.x + 2.0).max(52.0)
}

fn slider_row_impl(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    speed: f32,
    decimals: usize,
    clamp_existing: bool,
) -> bool {
    let mut changed = false;
    let field_width = drag_field_width(ui, &range, decimals);
    let narrow = ui.available_width() < 240.0;
    let out_of_range = !clamp_existing && (*value < *range.start() || *value > *range.end());
    let hint = if out_of_range {
        format!(
            "{label}\nCurrent value {value:.2} is outside the {:.0} – {:.0} authoring range. \
             Editing it clamps to the range.",
            range.start(),
            range.end()
        )
    } else {
        format!(
            "{label}\nRange {:.prec$} – {:.prec$}. Drag the number or double-click it to type \
             an exact value. Shift-drag the slider for fine control (Ctrl/Alt: ultra-fine).",
            range.start(),
            range.end(),
            prec = decimals
        )
    };
    if narrow {
        ui.label(egui::RichText::new(label).size(TEXT_SMALL).color(MUTED));
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let slider_width = (ui.available_width() - field_width - 6.0).max(32.0);
            let (slider_changed, slider) = m3_slider(ui, value, range.clone(), slider_width);
            changed |= slider_changed;
            slider.on_hover_text(&hint);
            let field = ui.scope(|ui| {
                if out_of_range {
                    ui.visuals_mut().override_text_color = Some(WARNING);
                }
                ui.add_sized(
                    [field_width, 22.0],
                    egui::DragValue::new(value)
                        .range(range)
                        .clamp_existing_to_range(clamp_existing)
                        .speed(speed)
                        .fixed_decimals(decimals),
                )
            });
            changed |= field.inner.changed();
            field.response.on_hover_text(&hint);
        });
        return changed;
    }
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        field_label(ui, label);
        let slider_width = (ui.available_width() - field_width - 6.0).max(56.0);
        let (slider_changed, slider) = m3_slider(ui, value, range.clone(), slider_width);
        changed |= slider_changed;
        slider.on_hover_text(&hint);
        let field = ui.scope(|ui| {
            if out_of_range {
                ui.visuals_mut().override_text_color = Some(WARNING);
            }
            ui.add_sized(
                [field_width, 24.0],
                egui::DragValue::new(value)
                    .range(range)
                    .clamp_existing_to_range(clamp_existing)
                    .speed(speed)
                    .fixed_decimals(decimals),
            )
        });
        changed |= field.inner.changed();
        field.response.on_hover_text(&hint);
    });
    changed
}

pub(crate) fn draw_profile_curve(painter: &egui::Painter, rect: egui::Rect, profile: &Profile) {
    painter.rect_filled(rect, egui::CornerRadius::same(RADIUS_SM), CONTAINER_HIGH);
    let inner = rect.shrink(4.0);
    let points: Vec<egui::Pos2> = (0..=20)
        .map(|index| {
            let t = index as f32 / 20.0;
            egui::pos2(
                inner.left() + t * inner.width(),
                inner.bottom() - profile.sample(t) * inner.height(),
            )
        })
        .collect();
    painter.add(egui::Shape::line(
        points,
        egui::Stroke::new(1.4_f32, ACCENT),
    ));
}
/// Material 3 filled button, used for the one primary action (Export).
/// Hover/focus/press are state layers over the accent fill, not a new colour.
pub(crate) fn filled_button(ui: &mut egui::Ui, label: &str, height: f32) -> egui::Response {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
    let enabled = ui.is_enabled();
    let painter = ui.painter();
    if !enabled {
        painter.rect_filled(
            rect,
            egui::CornerRadius::same(RADIUS_SM),
            state_layer(CONTAINER_LOW, ON_SURFACE, 0.12),
        );
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(TEXT_LABEL),
            TEXT_DISABLED,
        );
        return response;
    }
    painter.rect_filled(rect, egui::CornerRadius::same(RADIUS_MD), PRIMARY);
    let overlay = if response.is_pointer_button_down_on() {
        PRESS_ALPHA
    } else if response.has_focus() {
        FOCUS_ALPHA
    } else if response.hovered() {
        HOVER_ALPHA
    } else {
        0.0
    };
    if overlay > 0.0 {
        painter.rect_filled(
            rect,
            egui::CornerRadius::same(RADIUS_MD),
            state_layer(PRIMARY, ON_PRIMARY, overlay),
        );
    }
    if response.has_focus() {
        painter.rect_stroke(
            rect.expand(2.0),
            egui::CornerRadius::same(12),
            egui::Stroke::new(2.0_f32, PRIMARY),
            egui::StrokeKind::Outside,
        );
    }
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        bold(15.0),
        ON_PRIMARY,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Material 3 text button: no fill at rest, tonal hover layer.
pub(crate) fn text_button(
    ui: &mut egui::Ui,
    label: &str,
    width: f32,
    height: f32,
    danger: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
    let enabled = ui.is_enabled();
    if response.hovered() && enabled {
        ui.painter().rect_filled(
            rect,
            egui::CornerRadius::same(RADIUS_SM),
            hover_layer(CONTAINER_LOW),
        );
    }
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect.expand(1.0),
            egui::CornerRadius::same(9),
            egui::Stroke::new(2.0_f32, PRIMARY),
            egui::StrokeKind::Outside,
        );
    }
    let color = if !enabled {
        TEXT_DISABLED
    } else if danger && response.hovered() {
        DANGER
    } else {
        ON_SURFACE_VAR
    };
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(TEXT_SMALL),
        color,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Map a relative slider drag to a value. `fine` scales pointer movement:
/// 1.0 is normal, 0.1 is Shift-fine, 0.01 is Ctrl/Alt-ultra-fine.
#[allow(clippy::too_many_arguments)]
fn slider_drag_value(
    start_value: f32,
    start_x: f32,
    pointer_x: f32,
    rail_width: f32,
    span: f32,
    fine: f32,
    lo: f32,
    hi: f32,
) -> f32 {
    (start_value + (pointer_x - start_x) / rail_width.max(1.0) * span * fine).clamp(lo, hi)
}

/// Material 3 slider: 4 px track, primary active part, 16 px filled thumb.
/// Returns whether the value changed plus the response for tooltips.
pub(crate) fn m3_slider(
    ui: &mut egui::Ui,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    width: f32,
) -> (bool, egui::Response) {
    let (lo, hi) = (*range.start(), *range.end());
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(width.max(40.0), 22.0),
        egui::Sense::click_and_drag(),
    );
    let rail = egui::Rect::from_center_size(rect.center(), egui::vec2(rect.width(), 4.0));
    let span = (hi - lo).max(f32::EPSILON);
    let t = ((*value - lo) / span).clamp(0.0, 1.0);
    // Reference slider: a 4 px rail with a 15 px thumb that travels between
    // the thumb-radius ends. The 22 px allocation stays as the hit target.
    const THUMB_RADIUS: f32 = 7.5;
    let center = egui::pos2(
        rail.left() + THUMB_RADIUS + (rail.width() - THUMB_RADIUS * 2.0).max(0.0) * t,
        rail.center().y,
    );
    let fill_end = rail.left() + rail.width() * t;

    let painter = ui.painter();
    painter.rect_filled(rail, egui::CornerRadius::same(2), TRACK);
    painter.rect_filled(
        egui::Rect::from_min_max(rail.left_top(), egui::pos2(fill_end, rail.bottom())),
        egui::CornerRadius::same(2),
        PRIMARY,
    );
    // Chromium's thumb: white centre inside a 3 px accent ring.
    painter.circle_filled(center, THUMB_RADIUS, PRIMARY);
    painter.circle_filled(center, THUMB_RADIUS - 3.0, egui::Color32::WHITE);
    if response.has_focus() {
        painter.circle_stroke(
            center,
            THUMB_RADIUS + 2.0,
            egui::Stroke::new(2.0_f32, PRIMARY),
        );
    }

    let mut changed = false;
    let fine = ui.input(|input| {
        if input.modifiers.alt || input.modifiers.command {
            0.01
        } else if input.modifiers.shift {
            0.1
        } else {
            1.0
        }
    });
    let drag_id = response.id.with("m3_slider_drag");
    if response.drag_started() {
        if let Some(pointer) = response.interact_pointer_pos() {
            ui.data_mut(|data| data.insert_temp(drag_id, (*value, pointer.x, fine)));
        }
    } else if response.is_pointer_button_down_on() && !response.dragged() && fine >= 1.0 {
        // A press without a drag is a click-to-set. Fine-drag modifiers skip the
        // jump so a Shift/Ctrl/Alt drag starts from the current value.
        if let Some(pointer) = response.interact_pointer_pos() {
            let next = ((pointer.x - rail.left()) / rail.width().max(1.0)).clamp(0.0, 1.0);
            let next = lo + next * span;
            if (*value - next).abs() > f32::EPSILON {
                *value = next;
                changed = true;
            }
        }
    }
    if response.dragged() {
        if let Some(pointer) = response.interact_pointer_pos() {
            // Relative dragging keeps the thumb under the pointer and lets the
            // modifier scale the movement instead of snapping to the pointer.
            // Shift is 10x finer, Ctrl/Alt is 100x finer. Rebase when the modifier
            // changes mid-drag so the value never jumps.
            let mut state = ui.data(|data| data.get_temp::<(f32, f32, f32)>(drag_id));
            if state.is_none() {
                // `drag_started` can be missed on the first dragged frame; seed
                // the baseline here so the drag still tracks.
                let initial = (*value, pointer.x, fine);
                ui.data_mut(|data| data.insert_temp(drag_id, initial));
                state = Some(initial);
            }
            let (mut start_value, mut start_x, mut start_fine) = state.unwrap();
            if (start_fine - fine).abs() > f32::EPSILON {
                start_value = *value;
                start_x = pointer.x;
                start_fine = fine;
                ui.data_mut(|data| data.insert_temp(drag_id, (start_value, start_x, start_fine)));
            }
            let next = slider_drag_value(
                start_value,
                start_x,
                pointer.x,
                rail.width(),
                span,
                fine,
                lo,
                hi,
            );
            if (*value - next).abs() > f32::EPSILON {
                *value = next;
                changed = true;
            }
        }
    }
    if response.drag_stopped() {
        ui.data_mut(|data| data.remove::<(f32, f32, f32)>(drag_id));
    }
    if response.has_focus() {
        let step = span / 100.0
            * if ui.input(|i| i.modifiers.shift) {
                10.0
            } else {
                1.0
            };
        let increase =
            ui.input(|i| i.key_pressed(egui::Key::ArrowRight) || i.key_pressed(egui::Key::ArrowUp));
        let decrease = ui
            .input(|i| i.key_pressed(egui::Key::ArrowLeft) || i.key_pressed(egui::Key::ArrowDown));
        if increase {
            *value = (*value + step).clamp(lo, hi);
            changed = true;
        } else if decrease {
            *value = (*value - step).clamp(lo, hi);
            changed = true;
        }
    }
    (
        changed,
        response.on_hover_cursor(egui::CursorIcon::PointingHand),
    )
}

/// Material 3 checkbox: 2 px outline when off, primary fill + check when on.
pub(crate) fn m3_checkbox(ui: &mut egui::Ui, value: &mut bool, label: &str) -> egui::Response {
    let (rect, mut response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 30.0), egui::Sense::click());
    if response.clicked() {
        *value = !*value;
        response.mark_changed();
    }
    let painter = ui.painter();
    if response.hovered() {
        painter.rect_filled(
            rect,
            egui::CornerRadius::same(RADIUS_SM),
            hover_layer(CONTAINER_LOW),
        );
    }
    let box_rect = egui::Rect::from_center_size(
        egui::pos2(rect.left() + 11.0, rect.center().y),
        egui::vec2(18.0, 18.0),
    );
    if *value {
        painter.rect_filled(box_rect, egui::CornerRadius::same(RADIUS_XS), PRIMARY);
        let c = box_rect.center();
        let stroke = egui::Stroke::new(2.0_f32, ON_PRIMARY);
        painter.line_segment(
            [
                egui::pos2(c.x - 4.5, c.y + 0.5),
                egui::pos2(c.x - 1.0, c.y + 4.0),
            ],
            stroke,
        );
        painter.line_segment(
            [
                egui::pos2(c.x - 1.0, c.y + 4.0),
                egui::pos2(c.x + 4.5, c.y - 4.0),
            ],
            stroke,
        );
    } else {
        painter.rect_stroke(
            box_rect,
            egui::CornerRadius::same(RADIUS_XS),
            egui::Stroke::new(2.0_f32, OUTLINE),
            egui::StrokeKind::Inside,
        );
    }
    if response.has_focus() {
        painter.rect_stroke(
            box_rect.expand(2.0),
            egui::CornerRadius::same(6),
            egui::Stroke::new(2.0_f32, PRIMARY),
            egui::StrokeKind::Outside,
        );
    }
    painter.text(
        egui::pos2(box_rect.right() + 10.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(TEXT_SMALL),
        ON_SURFACE,
    );
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, *value, label)
    });
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Segmented control in the reference's style: one inset track with equal
/// options and a solid accent segment. Returns the newly clicked index.
pub(crate) fn segmented_control(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    options: &[&str],
    selected: Option<usize>,
) -> Option<usize> {
    if options.is_empty() {
        return None;
    }
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 30.0), egui::Sense::hover());
    ui.painter().rect_filled(
        rect,
        egui::CornerRadius::same(RADIUS_SM + 2),
        INSPECTOR_FIELD,
    );
    ui.painter().rect_stroke(
        rect,
        egui::CornerRadius::same(RADIUS_SM + 2),
        egui::Stroke::new(1.0_f32, INSPECTOR_LINE),
        egui::StrokeKind::Inside,
    );

    let inner = rect.shrink(3.0);
    let gap = 3.0;
    let segment_width =
        ((inner.width() - gap * (options.len() as f32 - 1.0)) / options.len() as f32).max(1.0);
    let mut clicked = None;
    for (index, label) in options.iter().enumerate() {
        let segment = egui::Rect::from_min_size(
            inner.min + egui::vec2(index as f32 * (segment_width + gap), 0.0),
            egui::vec2(segment_width, inner.height()),
        );
        let id = ui.id().with((&id_salt, index));
        let response = ui.interact(segment, id, egui::Sense::click());
        let active = selected == Some(index);
        let painter = ui.painter();
        if active {
            painter.rect_filled(segment, egui::CornerRadius::same(RADIUS_SM), PRIMARY);
        } else if response.hovered() {
            painter.rect_filled(
                segment,
                egui::CornerRadius::same(RADIUS_SM),
                state_layer(INSPECTOR_FIELD, ON_SURFACE, HOVER_ALPHA),
            );
        }
        let ink = if active {
            ON_PRIMARY
        } else if response.hovered() {
            INSPECTOR_TEXT
        } else {
            INSPECTOR_MUTED
        };
        let font = if active {
            bold(12.0)
        } else {
            egui::FontId::proportional(12.0)
        };
        let galley = painter.layout_no_wrap((*label).to_owned(), font, ink);
        painter.galley(segment.center() - galley.size() * 0.5, galley, ink);
        if response.clicked() {
            clicked = Some(index);
        }
    }
    clicked
}

/// One corner of a dashed rounded rectangle, sampled as short segments.
fn push_arc(path: &mut Vec<egui::Pos2>, center: egui::Pos2, start: f32, end: f32, radius: f32) {
    const STEPS: usize = 4;
    for step in 0..=STEPS {
        let angle = start + (end - start) * step as f32 / STEPS as f32;
        path.push(egui::pos2(
            center.x + angle.cos() * radius,
            center.y + angle.sin() * radius,
        ));
    }
}

/// Dashed rounded outline built from a sampled perimeter. `Shape::dashed_line`
/// only takes a polyline, so corners are four short segments each.
pub(crate) fn dashed_round_rect(
    painter: &egui::Painter,
    rect: egui::Rect,
    radius: f32,
    stroke: egui::Stroke,
    dash: f32,
    gap: f32,
) {
    let radius = radius
        .min(rect.width() * 0.5)
        .min(rect.height() * 0.5)
        .max(0.0);
    let mut path = Vec::with_capacity(40);

    path.push(egui::pos2(rect.left() + radius, rect.top()));
    path.push(egui::pos2(rect.right() - radius, rect.top()));
    push_arc(
        &mut path,
        egui::pos2(rect.right() - radius, rect.top() + radius),
        -std::f32::consts::FRAC_PI_2,
        0.0,
        radius,
    );
    path.push(egui::pos2(rect.right(), rect.bottom() - radius));
    push_arc(
        &mut path,
        egui::pos2(rect.right() - radius, rect.bottom() - radius),
        0.0,
        std::f32::consts::FRAC_PI_2,
        radius,
    );
    path.push(egui::pos2(rect.left() + radius, rect.bottom()));
    push_arc(
        &mut path,
        egui::pos2(rect.left() + radius, rect.bottom() - radius),
        std::f32::consts::FRAC_PI_2,
        std::f32::consts::PI,
        radius,
    );
    path.push(egui::pos2(rect.left(), rect.top() + radius));
    push_arc(
        &mut path,
        egui::pos2(rect.left() + radius, rect.top() + radius),
        std::f32::consts::PI,
        std::f32::consts::PI * 1.5,
        radius,
    );
    if let Some(first) = path.first().copied() {
        path.push(first);
    }
    painter.extend(egui::Shape::dashed_line(&path, stroke, dash, gap));
}

/// Dashed import drop zone (`drop` in the reference).
pub(crate) fn dashed_drop_zone(
    ui: &mut egui::Ui,
    icon: Icon,
    title: &str,
    subtitle: &str,
) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 78.0), egui::Sense::click());
    let hovered = response.hovered();
    let painter = ui.painter();
    if hovered {
        painter.rect_filled(
            rect,
            egui::CornerRadius::same(RADIUS_MD),
            INSPECTOR_FIELD_HOVER,
        );
    }
    dashed_round_rect(
        painter,
        rect,
        RADIUS_MD as f32,
        egui::Stroke::new(
            1.5_f32,
            if hovered {
                PRIMARY
            } else {
                INSPECTOR_LINE_STRONG
            },
        ),
        6.0,
        4.0,
    );

    let icon_rect = egui::Rect::from_center_size(
        egui::pos2(rect.center().x, rect.top() + 24.0),
        egui::vec2(22.0, 22.0),
    );
    draw_icon(painter, icon_rect, icon, PRIMARY);

    let title_ink = if hovered { PRIMARY } else { INSPECTOR_TEXT };
    let galley = painter.layout_no_wrap(title.to_owned(), bold(13.0), title_ink);
    painter.galley(
        egui::pos2(rect.center().x - galley.size().x * 0.5, rect.top() + 40.0),
        galley,
        title_ink,
    );
    let hint = painter.layout_no_wrap(
        subtitle.to_owned(),
        egui::FontId::proportional(11.5),
        INSPECTOR_DIM,
    );
    painter.galley(
        egui::pos2(rect.center().x - hint.size().x * 0.5, rect.top() + 58.0),
        hint,
        INSPECTOR_DIM,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Dashed pill for the `+ Array` style add-operation actions.
pub(crate) fn dashed_chip(
    ui: &mut egui::Ui,
    label: &str,
    active: bool,
    width: f32,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 30.0), egui::Sense::click());
    let hovered = response.hovered();
    let painter = ui.painter();
    let strong = active || hovered;
    if active || hovered {
        painter.rect_filled(
            rect,
            egui::CornerRadius::same(15),
            if active {
                ACCENT_WASH
            } else {
                egui::Color32::from_rgba_unmultiplied(PRIMARY.r(), PRIMARY.g(), PRIMARY.b(), 18)
            },
        );
    }
    if strong {
        painter.rect_stroke(
            rect,
            egui::CornerRadius::same(15),
            egui::Stroke::new(1.0_f32, PRIMARY),
            egui::StrokeKind::Inside,
        );
    } else {
        dashed_round_rect(
            painter,
            rect,
            15.0,
            egui::Stroke::new(1.0_f32, INSPECTOR_LINE_STRONG),
            4.0,
            3.0,
        );
    }
    let ink = if strong { PRIMARY } else { INSPECTOR_MUTED };
    let galley = painter.layout_no_wrap(label.to_owned(), egui::FontId::proportional(12.5), ink);
    painter.galley(rect.center() - galley.size() * 0.5, galley, ink);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub(crate) fn labeled_combo(
    ui: &mut egui::Ui,
    label: &str,
    id: &str,
    selected: &str,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    ui.label(
        egui::RichText::new(label)
            .size(TEXT_SMALL)
            .color(INSPECTOR_MUTED),
    );
    egui::ComboBox::from_id_salt(id)
        .width(ui.available_width())
        .selected_text(selected)
        .show_ui(ui, add_contents);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fine_slider_drag_scales_the_pointer_delta() {
        let coarse = slider_drag_value(0.0, 0.0, 100.0, 200.0, 1024.0, 1.0, -1024.0, 1024.0);
        let fine = slider_drag_value(0.0, 0.0, 100.0, 200.0, 1024.0, 0.1, -1024.0, 1024.0);
        let ultra = slider_drag_value(0.0, 0.0, 100.0, 200.0, 1024.0, 0.01, -1024.0, 1024.0);
        assert!((coarse - 512.0).abs() < 1e-3);
        assert!((fine - 51.2).abs() < 1e-3);
        assert!((ultra - 5.12).abs() < 1e-3);

        let clamped = slider_drag_value(0.0, 0.0, 1_000.0, 200.0, 1024.0, 1.0, -1024.0, 1024.0);
        assert_eq!(clamped, 1024.0);
    }

    #[test]
    fn slider_drag_tracks_the_pointer() {
        let ctx = egui::Context::default();
        let mut value = 0.0_f32;
        let frame = |events: Vec<egui::Event>, value: &mut f32| {
            let mut raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 100.0),
                )),
                ..Default::default()
            };
            raw.events = events;
            let output = ctx.run_ui(raw, |ui| {
                ui.set_width(300.0);
                m3_slider(ui, value, -100.0..=100.0, 300.0);
            });
            output.drop_without_applying_deltas();
        };
        let pos = |x: f32| egui::pos2(x, 11.0);
        frame(vec![], &mut value);
        frame(
            vec![
                egui::Event::PointerMoved(pos(20.0)),
                egui::Event::PointerButton {
                    pos: pos(20.0),
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                },
            ],
            &mut value,
        );
        let pressed = value;
        frame(vec![egui::Event::PointerMoved(pos(120.0))], &mut value);
        let drag_started = value;
        frame(vec![egui::Event::PointerMoved(pos(220.0))], &mut value);
        assert!(
            value > drag_started + 10.0,
            "drag did not move the value: pressed {pressed}, start {drag_started}, now {value}"
        );
    }

    #[test]
    fn viewport_tabs_use_the_reference_line_box() {
        let ctx = egui::Context::default();
        let mut single = egui::Vec2::ZERO;
        let mut double = egui::Vec2::ZERO;
        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            single = bar_tab(ui, "Split", false).rect.size();
            double = bar_tab(ui, "3D\nView", false).rect.size();
        });
        output.drop_without_applying_deltas();
        // 10 px padding top + bottom and a 14 px x 1.15 line box per line.
        let line = 14.0_f32 * 1.15;
        // egui snaps allocated rects to its rounding grid, so allow one step.
        assert!(
            (single.y - (line + 20.0)).abs() < 0.05,
            "single {} vs {}",
            single.y,
            line + 20.0
        );
        assert!(
            (double.y - (2.0 * line + 20.0)).abs() < 0.05,
            "double {} vs {}",
            double.y,
            2.0 * line + 20.0
        );
        assert!(double.x > 36.0, "the tab keeps 18 px padding on each side");
    }

    #[test]
    fn tool_cells_match_the_reference_size() {
        let ctx = egui::Context::default();
        let mut cell = egui::Vec2::ZERO;
        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            cell = rail_tool(ui, true, "select", "Select", "tip").rect.size();
        });
        output.drop_without_applying_deltas();
        assert!((cell.x - 56.0).abs() < 0.01);
        assert!((cell.y - 51.4).abs() < 0.01);
    }

    #[test]
    fn a_bottom_inset_bar_leaves_a_strip_at_the_bottom() {
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(80.0, 36.0));
        let inner = inset_rect(rect, egui::vec2(0.0, -2.0));
        assert!((inner.left() - 0.0).abs() < 1e-3);
        assert!((inner.right() - 80.0).abs() < 1e-3);
        assert!((inner.top() - 0.0).abs() < 1e-3);
        assert!((inner.bottom() - 34.0).abs() < 1e-3);
    }

    #[test]
    fn a_left_inset_bar_leaves_a_strip_at_the_left() {
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(80.0, 36.0));
        let inner = inset_rect(rect, egui::vec2(3.0, 0.0));
        assert!((inner.left() - 3.0).abs() < 1e-3);
        assert!((inner.right() - 80.0).abs() < 1e-3);
        assert!((inner.top() - 0.0).abs() < 1e-3);
        assert!((inner.bottom() - 36.0).abs() < 1e-3);
    }

    #[test]
    fn the_channel_select_is_230_wide() {
        let ctx = egui::Context::default();
        let mut width = 0.0;
        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            width = bar_select(ui, "probe", "Normal (XYZ Tangent)", 230.0, |_ui| {})
                .rect
                .width();
        });
        output.drop_without_applying_deltas();
        assert!((width - 230.0).abs() < 1.0, "select measured {width}");
    }
}
