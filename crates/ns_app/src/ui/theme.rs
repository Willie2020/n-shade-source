//! Shared colour roles, surfaces, and egui theme setup.
//!
//! Material 3 direction (see `docs/UI_IMPROVEMENT_MATERIAL_EGUI.md`): structure
//! comes from tonal surfaces and state layers, not from outlines.
//!
//! Palette: the Vibrant Dark System — a high-contrast dark ladder with a cyan
//! accent. The role constants below are the single source of truth; panels
//! should not hard-code hex values.

use super::*;

// ---------------------------------------------------------------------------
// Colour roles (Vibrant Dark System)
// ---------------------------------------------------------------------------

/// Viewport / window background (`surface-container-lowest`).
pub(crate) const SURFACE_LOWEST: egui::Color32 = egui::Color32::from_rgb(0x08, 0x0F, 0x12);
/// App background behind the shell (`surface` / `background`).
pub(crate) const SURFACE: egui::Color32 = egui::Color32::from_rgb(0x0E, 0x14, 0x17);
/// Panels: layers, properties, rail, top and status bars (`surface-container-low`).
pub(crate) const CONTAINER_LOW: egui::Color32 = egui::Color32::from_rgb(0x16, 0x1D, 0x1F);
/// Cards, rows and buttons (`surface-container`).
pub(crate) const CONTAINER: egui::Color32 = egui::Color32::from_rgb(0x1A, 0x21, 0x23);
/// Raised step: menus, active tabs, pressed chips (`surface-container-high`).
pub(crate) const CONTAINER_HIGH: egui::Color32 = egui::Color32::from_rgb(0x24, 0x2B, 0x2E);
/// Highest tonal step: text fields, wells and inactive rails
/// (`surface-container-highest`).
pub(crate) const CONTAINER_HIGHEST: egui::Color32 = egui::Color32::from_rgb(0x2F, 0x36, 0x39);

pub(crate) const ON_SURFACE: egui::Color32 = egui::Color32::from_rgb(0xDD, 0xE3, 0xE7);
/// Muted body copy and labels (`on-surface-variant`).
pub(crate) const ON_SURFACE_VAR: egui::Color32 = egui::Color32::from_rgb(0xBB, 0xC9, 0xCE);
/// Small mono labels and captions (`outline`).
pub(crate) const FAINT: egui::Color32 = egui::Color32::from_rgb(0x85, 0x93, 0x98);
/// Every border in the shell (`outline`).
pub(crate) const OUTLINE: egui::Color32 = egui::Color32::from_rgb(0x85, 0x93, 0x98);
/// The hairline between panels, cards and rows (`outline-variant`).
pub(crate) const OUTLINE_VARIANT: egui::Color32 = egui::Color32::from_rgb(0x3C, 0x49, 0x4D);

/// Projection cage axes, matching the usual X/Y/Z red/green/blue convention.
pub(crate) const AXIS_X: egui::Color32 = egui::Color32::from_rgb(0xEB, 0x57, 0x4D);
pub(crate) const AXIS_Y: egui::Color32 = egui::Color32::from_rgb(0x6B, 0xD1, 0x70);
pub(crate) const AXIS_Z: egui::Color32 = egui::Color32::from_rgb(0x61, 0x9E, 0xF0);

/// Accent: actions, toggles, slider fills, UI selection (`primary-container`).
pub(crate) const PRIMARY: egui::Color32 = egui::Color32::from_rgb(0x00, 0xD6, 0xFC);
/// Solid accent fill.
pub(crate) const PRIMARY_CONTAINER: egui::Color32 = egui::Color32::from_rgb(0x00, 0xD6, 0xFC);
/// Text and glyphs on a solid accent fill (`on-primary-container`).
pub(crate) const ON_PRIMARY: egui::Color32 = egui::Color32::from_rgb(0x00, 0x36, 0x41);
/// 14% accent wash for ghost buttons and active tool tiles
/// (`PRIMARY` over `CONTAINER_LOW`).
pub(crate) const ACCENT_WASH: egui::Color32 = egui::Color32::from_rgb(0x13, 0x37, 0x3E);
/// Cool secondary tone for canvas overlays the chrome never uses (`secondary`).
pub(crate) const PRIMARY_COOL: egui::Color32 = egui::Color32::from_rgb(0x9B, 0xCF, 0xDF);
/// 2D canvas selection chrome. A warm orange complement to the cyan accent:
/// it separates the selection from cyan authoring previews (pen, lasso,
/// guides) and stays legible on blue/purple normal maps.
pub(crate) const SELECTION: egui::Color32 = egui::Color32::from_rgb(0xFF, 0xA2, 0x4A);
/// Dark under-stroke behind selection lines so they stay visible over light
/// normal maps as well as on the dark canvas.
pub(crate) const SELECTION_HALO: egui::Color32 = egui::Color32::from_rgb(0x1B, 0x0F, 0x06);

/// Tonal fill for selected rows, chips and segments (`secondary-container`).
pub(crate) const SECONDARY_CONTAINER: egui::Color32 = egui::Color32::from_rgb(0x13, 0x4D, 0x5B);
/// Text and icons on the tonal selected surface (`on-secondary-container`).
pub(crate) const ON_SECONDARY_CONTAINER: egui::Color32 = egui::Color32::from_rgb(0x8A, 0xBD, 0xCD);

pub(crate) const ERROR: egui::Color32 = egui::Color32::from_rgb(0xFF, 0xB4, 0xAB);
/// Destructive hover wash (`error-container` at 35 % over `CONTAINER_LOW`).
pub(crate) const DANGER_WASH: egui::Color32 = egui::Color32::from_rgb(0x42, 0x13, 0x18);
pub(crate) const WARN: egui::Color32 = egui::Color32::from_rgb(0xF2, 0xC1, 0x4E);
pub(crate) const OK: egui::Color32 = egui::Color32::from_rgb(0x2F, 0xD2, 0x7A);

/// Outline shared by fields, combos and secondary buttons.
pub(crate) const CONTROL_BORDER: egui::Color32 = OUTLINE_VARIANT;
/// Inactive slider rail.
pub(crate) const TRACK: egui::Color32 = egui::Color32::from_rgb(0x2F, 0x36, 0x39);

// ---------------------------------------------------------------------------
// Inspector palette (right dock)
// ---------------------------------------------------------------------------
// Mirrors `nshade-right-panel.html`: a darker panel, raised cards with a
// hairline, and inset fields, while keeping the app's cyan accent.

/// Right-dock surface (`--panel` in the reference).
pub(crate) const INSPECTOR_BG: egui::Color32 = egui::Color32::from_rgb(0x0D, 0x17, 0x1C);
/// Raised card on the inspector surface (`--card`).
pub(crate) const INSPECTOR_CARD: egui::Color32 = egui::Color32::from_rgb(0x12, 0x1E, 0x24);
/// Inset field and segment track (`#00000040` over the panel).
pub(crate) const INSPECTOR_FIELD: egui::Color32 = egui::Color32::from_rgb(0x08, 0x11, 0x15);
/// Hovered field/drop surface.
pub(crate) const INSPECTOR_FIELD_HOVER: egui::Color32 = egui::Color32::from_rgb(0x10, 0x1C, 0x22);
/// Card and section hairline (`--line`).
pub(crate) const INSPECTOR_LINE: egui::Color32 = egui::Color32::from_rgb(0x1E, 0x2F, 0x37);
/// Control border (`--line2`).
pub(crate) const INSPECTOR_LINE_STRONG: egui::Color32 = egui::Color32::from_rgb(0x2A, 0x3F, 0x49);
/// Primary inspector text (`--text`).
pub(crate) const INSPECTOR_TEXT: egui::Color32 = egui::Color32::from_rgb(0xE6, 0xF0, 0xF3);
/// Labels and hints (`--mute`).
pub(crate) const INSPECTOR_MUTED: egui::Color32 = egui::Color32::from_rgb(0x8A, 0xA0, 0xA9);
/// Disabled and secondary glyphs (`--dim`).
pub(crate) const INSPECTOR_DIM: egui::Color32 = egui::Color32::from_rgb(0x5D, 0x72, 0x7B);

// ---------------------------------------------------------------------------
// Backwards-compatible aliases used across the panels
// ---------------------------------------------------------------------------

pub(crate) const APP_BG: egui::Color32 = SURFACE;
pub(crate) const INPUT_BG: egui::Color32 = CONTAINER_HIGHEST;
/// Hover state layer over the control surface.
pub(crate) const HOVER_BG: egui::Color32 = CONTAINER_HIGH;
pub(crate) const SELECTED_BG: egui::Color32 = SECONDARY_CONTAINER;
/// Dividers only; resting controls have no stroke.
pub(crate) const BORDER: egui::Color32 = OUTLINE_VARIANT;
pub(crate) const DIVIDER: egui::Color32 = OUTLINE_VARIANT;
pub(crate) const TEXT: egui::Color32 = ON_SURFACE;
pub(crate) const MUTED: egui::Color32 = ON_SURFACE_VAR;
/// Disabled glyphs and labels (`neutral`, dimmer than captions).
pub(crate) const TEXT_DISABLED: egui::Color32 = egui::Color32::from_rgb(0x71, 0x78, 0x7B);
pub(crate) const ACCENT: egui::Color32 = PRIMARY;
pub(crate) const ACCENT_SOFT: egui::Color32 = ACCENT_WASH;
/// Text and icons on the tonal selected surface.
pub(crate) const ACCENT_TEXT: egui::Color32 = ON_SECONDARY_CONTAINER;
pub(crate) const SUCCESS: egui::Color32 = OK;
pub(crate) const WARNING: egui::Color32 = WARN;
pub(crate) const DANGER: egui::Color32 = ERROR;

// ---------------------------------------------------------------------------
// State layers (Material 3)
// ---------------------------------------------------------------------------

pub(crate) const HOVER_ALPHA: f32 = 0.08;
pub(crate) const FOCUS_ALPHA: f32 = 0.10;
pub(crate) const PRESS_ALPHA: f32 = 0.10;
pub(crate) const DRAG_ALPHA: f32 = 0.16;

/// Opaque blend of `content` over `base`, i.e. an M3 state layer.
pub(crate) fn state_layer(
    base: egui::Color32,
    content: egui::Color32,
    alpha: f32,
) -> egui::Color32 {
    let mix = |b: u8, c: u8| (b as f32 + (c as f32 - b as f32) * alpha).round() as u8;
    egui::Color32::from_rgb(
        mix(base.r(), content.r()),
        mix(base.g(), content.g()),
        mix(base.b(), content.b()),
    )
}

/// Hover layer for a control on `CONTAINER_LOW` / `CONTAINER_HIGHEST`.
pub(crate) fn hover_layer(base: egui::Color32) -> egui::Color32 {
    state_layer(base, ON_SURFACE, HOVER_ALPHA)
}

// ---------------------------------------------------------------------------
// Geometry and surfaces
// ---------------------------------------------------------------------------

pub(crate) const RADIUS_XS: u8 = 4;
pub(crate) const RADIUS_SM: u8 = 6;
pub(crate) const RADIUS_MD: u8 = 10;
pub(crate) const RADIUS_LG: u8 = 14;

// Type scale. Panels used to hard-code 8-11 px sizes, which made hints and
// labels unreadable next to 13 px widgets. Everything maps onto these steps and
// no readable text goes below 12 px (WCAG 1.4.3).
/// Hints, captions, secondary metadata.
pub(crate) const TEXT_CAPTION: f32 = 12.0;
/// Compact labels, chips, badges.
pub(crate) const TEXT_SMALL: f32 = 12.5;
/// Field labels and secondary controls.
pub(crate) const TEXT_LABEL: f32 = 13.0;

/// Named family for the semibold face (CSS 600).
pub(crate) const BOLD_FAMILY: &str = "bold";

/// True once the bundled faces are live. The semibold family only exists after
/// `set_fonts` has been applied, so text must not ask for it before then.
static FONTS_READY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Semibold face at `size`, falling back to the regular face until the fonts
/// are installed (they land before the first pass, so in practice this is only
/// the fallback for a missing install).
pub(crate) fn bold(size: f32) -> egui::FontId {
    if FONTS_READY.load(std::sync::atomic::Ordering::Relaxed) {
        egui::FontId::new(size, egui::FontFamily::Name(BOLD_FAMILY.into()))
    } else {
        egui::FontId::proportional(size)
    }
}

/// JetBrains Mono at `size`, for numbers, tags and meta lines.
pub(crate) fn mono(size: f32) -> egui::FontId {
    egui::FontId::monospace(size)
}

/// Installs the bundled faces: Inter for UI text, Inter SemiBold for headings
/// and Inter/JetBrains for meta. All are SIL OFL 1.1 (see `assets/fonts/OFL.txt`)
/// and embedded, so there is nothing to install on the machine.
///
/// Called from a startup system that runs before the first egui pass: egui only
/// applies `set_fonts` on the next pass, so installing during a pass would leave
/// the named family unbound for one frame and panic.
pub(crate) fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let regular =
        egui::FontData::from_static(include_bytes!("../../../../assets/fonts/Inter-Regular.ttf"));
    let semibold = egui::FontData::from_static(include_bytes!(
        "../../../../assets/fonts/Inter-SemiBold.ttf"
    ));
    let mono = egui::FontData::from_static(include_bytes!(
        "../../../../assets/fonts/JetBrainsMono-Regular.ttf"
    ));
    fonts
        .font_data
        .insert("inter".to_owned(), std::sync::Arc::new(regular));
    fonts
        .font_data
        .insert("inter_semibold".to_owned(), std::sync::Arc::new(semibold));
    fonts
        .font_data
        .insert("jetbrains".to_owned(), std::sync::Arc::new(mono));

    // Proportional: Inter, then egui's bundled emoji/script faces.
    let mut proportional = vec!["inter".to_owned()];
    if let Some(fallback) = fonts.families.get(&egui::FontFamily::Proportional) {
        proportional.extend(fallback.iter().cloned());
    }
    fonts
        .families
        .insert(egui::FontFamily::Proportional, proportional.clone());

    // Semibold, addressed by name so it can differ from the body face.
    let mut bold_family = vec!["inter_semibold".to_owned()];
    bold_family.extend(proportional.iter().cloned());
    fonts
        .families
        .insert(egui::FontFamily::Name(BOLD_FAMILY.into()), bold_family);

    // Monospace: JetBrains Mono, then whatever shipped with egui.
    let mut monospace = vec!["jetbrains".to_owned()];
    if let Some(fallback) = fonts.families.get(&egui::FontFamily::Monospace) {
        monospace.extend(fallback.iter().cloned());
    }
    fonts
        .families
        .insert(egui::FontFamily::Monospace, monospace);

    ctx.set_fonts(fonts);
    FONTS_READY.store(true, std::sync::atomic::Ordering::Relaxed);
}

pub(crate) fn apply_theme(ctx: &egui::Context) {
    // The tool rail uses the supplied SVG icon pack; install the loaders once.
    egui_extras::install_image_loaders(ctx);
    let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();
    let mut visuals = egui::Visuals::dark();

    // Surfaces and floating things.
    visuals.panel_fill = SURFACE;
    visuals.window_fill = CONTAINER_HIGH;
    visuals.window_stroke = egui::Stroke::NONE;
    visuals.window_corner_radius = egui::CornerRadius::same(RADIUS_LG);
    visuals.menu_corner_radius = egui::CornerRadius::same(RADIUS_SM);
    visuals.window_shadow = egui::epaint::Shadow {
        offset: [0, 8],
        blur: 24,
        spread: 0,
        color: egui::Color32::from_black_alpha(110),
    };
    visuals.popup_shadow = egui::epaint::Shadow {
        offset: [0, 4],
        blur: 12,
        spread: 0,
        color: egui::Color32::from_black_alpha(95),
    };
    visuals.extreme_bg_color = CONTAINER_HIGHEST;
    visuals.faint_bg_color = CONTAINER;
    visuals.override_text_color = None;
    visuals.selection.bg_fill = state_layer(CONTAINER, PRIMARY, 0.28);
    visuals.selection.stroke = egui::Stroke::new(2.0_f32, PRIMARY);
    visuals.hyperlink_color = PRIMARY;
    visuals.slider_trailing_fill = true;
    visuals.handle_shape = egui::style::HandleShape::Circle;
    visuals.indent_has_left_vline = false;

    // Controls carry a hairline outline: the reference UI separates fields and
    // buttons from their surface with a clean 1 px border, not a tonal jump.
    let control = |fill: egui::Color32, fg: egui::Color32, border: egui::Color32| {
        egui::style::WidgetVisuals {
            bg_fill: fill,
            weak_bg_fill: fill,
            bg_stroke: egui::Stroke::new(1.0_f32, border),
            corner_radius: egui::CornerRadius::same(RADIUS_SM),
            fg_stroke: egui::Stroke::new(1.5_f32, fg),
            expansion: 0.0,
        }
    };
    visuals.widgets.noninteractive = egui::style::WidgetVisuals {
        // `ui.separator()` draws with this stroke; the border budget allows at
        // most one divider per panel, so keep it quiet.
        bg_stroke: egui::Stroke::new(1.0_f32, OUTLINE_VARIANT),
        ..control(CONTAINER_LOW, ON_SURFACE, egui::Color32::TRANSPARENT)
    };
    visuals.widgets.inactive = control(CONTAINER, ON_SURFACE_VAR, CONTROL_BORDER);
    visuals.widgets.hovered = control(
        hover_layer(CONTAINER),
        ON_SURFACE,
        state_layer(CONTROL_BORDER, PRIMARY, 0.35),
    );
    visuals.widgets.active = control(CONTAINER_HIGH, ON_SURFACE, PRIMARY.gamma_multiply(0.55));
    visuals.widgets.open = control(
        state_layer(CONTAINER_HIGHEST, ON_SURFACE, FOCUS_ALPHA),
        ON_SURFACE,
        CONTROL_BORDER,
    );

    // `ctx.set_visuals` replaces style.visuals; keep the spacing/text we set.
    style.visuals = visuals;

    // Compact desktop density with M3 spacing.
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(12.0, 6.0);
    style.spacing.interact_size = egui::vec2(40.0, 32.0);
    style.spacing.slider_width = 140.0;
    style.spacing.window_margin = egui::Margin::same(16);
    style.spacing.menu_margin = egui::Margin::same(8);
    style.spacing.indent = 16.0;
    style.spacing.combo_height = 300.0;
    style.spacing.scroll = egui::style::ScrollStyle::floating();
    style.interaction.tooltip_delay = 0.4;
    style.spacing.tooltip_width = 340.0;

    style
        .text_styles
        .insert(egui::TextStyle::Heading, egui::FontId::proportional(16.0));
    style
        .text_styles
        .insert(egui::TextStyle::Body, egui::FontId::proportional(14.0));
    style
        .text_styles
        .insert(egui::TextStyle::Button, egui::FontId::proportional(13.0));
    style
        .text_styles
        .insert(egui::TextStyle::Small, egui::FontId::proportional(12.0));
    style
        .text_styles
        .insert(egui::TextStyle::Monospace, egui::FontId::monospace(13.0));
    ctx.all_styles_mut(move |target| *target = style.clone());
}

/// Vertical gap between rows inside a dock card.
pub(crate) const CARD_ROW_GAP: f32 = 8.0;

/// Raised inspector card (`--card` in the reference): a hairline border and
/// radius group a section, and the 14 px inset keeps controls off the edge.
pub(crate) fn card_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(INSPECTOR_CARD)
        .stroke(egui::Stroke::new(1.0_f32, INSPECTOR_LINE))
        .corner_radius(egui::CornerRadius::same(RADIUS_MD))
        .inner_margin(egui::Margin::same(14))
}

/// Right inspector dock: the darker reference surface with room for the tab
/// strip and a 14 px content gutter.
pub(crate) fn right_dock_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(INSPECTOR_BG)
        .inner_margin(egui::Margin::symmetric(14, 8))
}

/// Recolour standard widgets for the inspector scope. Custom widgets paint the
/// same constants directly; this covers DragValue, ComboBox, Checkbox and plain
/// Buttons so every right-panel tab shares one control surface.
pub(crate) fn inspector_style(ui: &mut egui::Ui) {
    let visuals = &mut ui.style_mut().visuals;
    visuals.extreme_bg_color = INSPECTOR_FIELD;
    visuals.selection.bg_fill = state_layer(INSPECTOR_CARD, PRIMARY, 0.22);
    visuals.selection.stroke = egui::Stroke::new(1.0_f32, PRIMARY);
    visuals.hyperlink_color = PRIMARY;

    visuals.widgets.inactive.weak_bg_fill = INSPECTOR_FIELD;
    visuals.widgets.inactive.bg_fill = INSPECTOR_FIELD;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, INSPECTOR_LINE);
    visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0_f32, INSPECTOR_TEXT);
    visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(RADIUS_SM);

    visuals.widgets.hovered.weak_bg_fill = INSPECTOR_FIELD_HOVER;
    visuals.widgets.hovered.bg_fill = INSPECTOR_FIELD_HOVER;
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, INSPECTOR_LINE_STRONG);
    visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0_f32, INSPECTOR_TEXT);
    visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(RADIUS_SM);

    visuals.widgets.active.weak_bg_fill = INSPECTOR_FIELD_HOVER;
    visuals.widgets.active.bg_fill = INSPECTOR_FIELD_HOVER;
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0_f32, PRIMARY.gamma_multiply(0.55));
    visuals.widgets.active.corner_radius = egui::CornerRadius::same(RADIUS_SM);

    visuals.widgets.open.weak_bg_fill = INSPECTOR_FIELD_HOVER;
    visuals.widgets.open.bg_fill = INSPECTOR_FIELD_HOVER;
    visuals.widgets.open.bg_stroke = egui::Stroke::new(1.0_f32, INSPECTOR_LINE_STRONG);
    visuals.widgets.open.corner_radius = egui::CornerRadius::same(RADIUS_SM);
}

/// A dock: a flat panel surface. The reference separates docks with a single
/// hairline, not with gaps and rounded corners.
pub(crate) fn dock_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(CONTAINER_LOW)
        .inner_margin(egui::Margin::symmetric(16, 12))
}

/// The viewport well, inset from the surrounding panels.
pub(crate) fn viewport_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(SURFACE_LOWEST)
        .inner_margin(egui::Margin::same(8))
}

/// Dock heading in sentence case ("Layers", "Export"), as in the reference.
pub(crate) fn dock_title(ui: &mut egui::Ui, icon: Icon, title: &str) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
        draw_icon(ui.painter(), rect, icon, PRIMARY);
        ui.add_space(3.0);
        ui.add(
            egui::Label::new(egui::RichText::new(title).font(bold(15.0)).color(TEXT)).truncate(),
        );
    });
}

/// Card heading in sentence case with the inspector's cyan icon (`sec-h` in
/// the reference). Cards are wayfinding, docks are titles.
pub(crate) fn section_title(ui: &mut egui::Ui, icon: Icon, title: &str) {
    section_title_with_pill(ui, icon, title, None);
}

/// Card heading plus the reference's state pill. A pill reports the state the
/// card is in ("Balanced", "On") without spending another row on it.
pub(crate) fn section_title_with_pill(
    ui: &mut egui::Ui,
    icon: Icon,
    title: &str,
    pill_state: Option<(&str, bool)>,
) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
        draw_icon(ui.painter(), rect, icon, PRIMARY);
        ui.add_space(4.0);
        ui.add(
            egui::Label::new(
                egui::RichText::new(title)
                    .font(bold(13.5))
                    .color(INSPECTOR_TEXT),
            )
            .truncate(),
        );
        if let Some((text, on)) = pill_state {
            ui.add_space(2.0);
            pill(ui, text, on);
        }
    });
}

/// Sentence-case sub-heading inside a card ("Maps", "Format", "Mask").
/// All-caps 10 px labels were the least readable text in the old shell.
pub(crate) fn subheading(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text.to_uppercase())
            .size(12.0)
            .color(FAINT),
    );
}
