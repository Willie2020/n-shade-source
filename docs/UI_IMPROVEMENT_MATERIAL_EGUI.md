# N-shade UI Improvement, Addendum: Material Design (desktop), low-border, Rust + egui

This document **adds to and overrides** `UI_IMPROVEMENT.md`. Read that file first for the audit, the functionality contract (its section 1) and the regression checklist (its section 9). Everything here is presentation only.

**Why this addendum exists:** the first pass leaned on outlines (control borders, chip outlines, selected-row outlines) to meet contrast rules. That made the UI busy. Material Design 3 (M3) gets structure from **tonal surfaces, spacing, shape and state layers**, and keeps borders for a few specific jobs. That is the direction here.

> **Version warning.** egui changes its API often (for example `Rounding` became `CornerRadius`, and `Margin`, `Shadow` and `Frame` constructors changed in recent releases). The current release I found documented is egui 0.34.x. All Rust below is written for that generation, but **compile it against your pinned version and fix names as needed**. Use egui's built-in style editor (`ctx.style_ui(...)`, or the Style/Visuals panel in `egui_demo_lib`) to see what each field changes.

---

## 1. What changes from the first document

| First document said | This addendum says | Reason |
|---|---|---|
| Control borders ≥ 3:1 (X4, section 4 tokens) | **Remove resting borders.** Identify controls by fill, label/icon contrast, and one extra cue (field underline, slider track and thumb) | Borders everywhere created noise |
| Selected layer row: 1 px accent border + accent bar | Selected = **secondary-container fill** (pill), no outline | M3 selected-item pattern |
| Export chips with check icon and outline | **Filled filter chips**: neutral fill when off, tonal-accent fill + check when on | Fewer lines, same clarity |
| Segmented control with outer border | Single container, selected segment filled, no outer border | Same |
| Accent for selected, on, primary | **Primary accent only for the one main action, sliders' active part, focus ring and indicators.** Selected/on states use a quieter tonal container | Reduces accent overuse (X1) |
| Dividers between sections | Sections separated by **space and tonal groups**; at most 1 divider per panel | Fewer lines |

What stays from the first document: labels, tooltips, shortcuts, undo toasts, lock toggle, memory warning colours, status messages, keyboard map, hit-target sizes (≥ 24 px, 32 px preferred), type minimum of 12 px, focus visibility.

---

## 2. Material 3 principles applied to a desktop tool

1. **Tonal layering instead of lines.** Depth comes from surface levels (lowest → highest). A panel is a slightly lighter surface than the app background. A group inside a panel is lighter again.
2. **State layers.** Hover, focus, pressed and dragged are shown by overlaying the content colour at a fixed opacity on the control: **hover 8%, focus 10%, pressed 10%, dragged 16%**. No hard colour swaps.
3. **Border budget.** Borders appear only where they carry meaning (section 5).
4. **Shape scale.** Consistent corner radii by role (section 4). Rounded containers, not rectangles with outlines.
5. **Colour roles, not colours.** Name colours by job (`primary`, `on_primary`, `secondary_container`, `surface_container_high`, `outline_variant`) and use only those names in code.
6. **Elevation.** Use **tonal** elevation first. Use **shadow only for things that float** (menus, popups, tooltips, toasts, dialogs).
7. **Desktop density.** M3 defaults target touch (48 dp). For desktop, use compact density: 32 px controls, 28–32 px list rows, 8 px gaps. Never go below 24×24 px targets.
8. **Layout.** On wide windows M3 uses a navigation rail on the left and side sheets. Your left tool rail is the navigation rail, the right Properties panel is a standard side sheet. Panels are rounded surfaces with 8 px gaps between them, not columns divided by lines.
9. **Motion is feedback.** 100–200 ms, ease-out, only to confirm a state change. Honour a "reduce motion" setting.
10. **Typography.** A small type scale with weight, not size, doing most of the hierarchy work.

---

## 3. Colour roles (Vibrant Dark System, shipped)

These are the shipped role values in `crates/ns_app/src/ui/theme.rs`, derived from the Vibrant Dark System: a high-contrast dark surface ladder with an energetic cyan accent. Keep the role names when retheming; for a mathematically exact M3 palette, generate one from the seed `#00D6FC` in Google's Material Theme Builder and map it back onto these roles.

| Role | Hex | Used for |
|---|---|---|
| `surface_container_lowest` | `#080F12` | Viewport well |
| `surface` (app background) | `#0E1417` | Gaps between panels |
| `surface_container_low` | `#161D1F` | Panels (layers, properties, rail, status bar) |
| `surface_container` | `#1A2123` | Grouped sections inside panels |
| `surface_container_high` | `#242B2E` | Chips (off), menus |
| `surface_container_highest` | `#2F3639` | Text fields, inactive slider track, stock buttons |
| `on_surface` | `#DDE3E7` | Primary text and icons |
| `on_surface_variant` | `#BBC9CE` | Secondary text, inactive icons |
| `outline` | `#859398` | Checkbox/radio outlines, captions |
| `outline_variant` | `#3C494D` | The single allowed divider |
| `primary` | `#00D6FC` | Export button, slider active track, focus ring, tab indicator |
| `on_primary` | `#003641` | Text on primary |
| `secondary_container` | `#134D5B` | Selected rail item, selected layer, selected chip, selected segment |
| `on_secondary_container` | `#8ABDCD` | Text/icons on the above |
| `error` / `error_container` | `#FFB4AB` / `#93000A` | Errors, destructive hover |
| `ok` / `warn` | `#2FD27A` / `#F2C14E` | Status, always with icon + text |

Contrast on the shipped pairs: `on_surface` on `surface_container_low` ≈ 13:1; `on_surface_variant` ≈ 10:1; `on_secondary_container` on `secondary_container` ≈ 4.6:1; `on_primary` on `primary` ≈ 7.5:1; `outline` captions on `surface` ≈ 5.9:1. The neutral `#71787B` is used only for disabled text (≈ 3.8:1).

**Light theme (optional):** keep the same role names, invert the surface ladder, and darken `primary` until text on it stays ≥ 4.5:1.

---

## 4. Shape, type, spacing, elevation tokens

| Token | Value | Use |
|---|---|---|
| `radius_xs` | 4 | Tooltips, small icon buttons |
| `radius_sm` | 8 | Buttons, chips, fields, rows |
| `radius_md` | 12 | Panels, groups, viewport container |
| `radius_lg` | 16 | Dialogs |
| Control height | 32 (compact), 24 minimum | Buttons, fields, rows 40 |
| Gap | 8 | Between panels, between controls: 8; between sections: 16–24 |
| Panel padding | 12 | |
| Elevation 0 | no shadow | Panels, groups |
| Elevation 2 | offset y4, blur 12, black 35% | Menus, popups, toasts |
| Elevation 3 | offset y8, blur 24, black 40% | Dialogs |

Type scale (IBM Plex Sans, or your current font): `title` 16 Medium, `body` 14 Regular, `label` 13 Medium, `caption` 12 Regular (minimum). Mono 13 for numbers if you want aligned values.

---

## 5. Border budget (the key rule)

**At rest, the UI should have zero outlines.** Allowed exceptions:

| Allowed | Spec |
|---|---|
| Keyboard focus ring | 2 px `primary`, 2 px offset, drawn **only** when focus is from keyboard navigation |
| Text field focus indicator | 2 px `primary` bottom line when focused (1 px `on_surface_variant` at rest, optional) |
| Checkbox / radio box | 2 px `outline` (the box is the control's identity) |
| One divider per panel | 1 px `outline_variant`, only between a fixed header and a scrolling list, and only while content is scrolled under it |
| Error state | 1 px `error` around an invalid field |
| Rare "outlined" button variant | 1 px `outline`, for a low-emphasis alternative action. Avoid unless needed |

### Replacement table (every border in your current screenshot)

| Element today | Replace with |
|---|---|
| Panel edges / column lines | 8 px gap, panel on `surface_container_low`, 12 px radius |
| Selected layer row outline | `secondary_container` pill fill |
| Row hover | 8% `on_surface` state layer |
| Layer thumbnail / mask slot borders | Filled `surface_container_highest` tile, 8 px radius; empty mask = dashed icon, no box |
| Inputs, DragValue, ComboBox outlines | `surface_container_highest` fill, no stroke, underline on focus |
| Export chips (orange outline = on) | Filter chips: off = `surface_container_high` fill; on = `secondary_container` fill + ✓ icon |
| Normal Y segmented buttons | One `surface_container_high` container, selected half `secondary_container` |
| Tabs | M3 primary tabs: no boxes, 3 px `primary` indicator under the active tab, optional 1 divider below the row |
| Buttons | Filled (Export), Tonal (secondary actions), Text (tertiary). No outlines |
| Toggle (Visible, Carve) | M3 switch: 40×22 track, thumb grows when on |
| Sliders | 4 px track: `primary` active part, `surface_container_highest` inactive part, 16 px round thumb |
| Properties sections | `surface_container` rounded groups with a 14 px section title; no dividers |
| Collapsible headers | Title text + chevron, 8% hover layer, no frame |
| Tool rail buttons | 48×48 icon + label; selected = 40×28 `secondary_container` pill behind the icon (M3 rail indicator) |
| Status bar | `surface_container_low`, no top line |
| Viewport frame | `surface_container_lowest` rounded 12 px container, inset 8 px |
| Menus, popups, tooltips | `surface_container_high` + elevation 2 shadow, no stroke (tooltips: inverse surface) |

---

## 6. egui implementation

### 6.1 Project layout

```text
src/ui/
  theme/
    mod.rs        // apply(ctx), re-exports
    colors.rs     // role constants (section 3)
    tokens.rs     // radii, spacing, durations
    state.rs      // state-layer helper
    fonts.rs      // font + icon-font install
  widgets/
    button.rs     // filled / tonal / text
    chip.rs       // filter chip
    segmented.rs
    switch.rs
    nav_rail.rs
    layer_row.rs
    field.rs      // filled text field with underline
  panels/         // layers.rs, export.rs, properties.rs, rail.rs, status.rs
```

### 6.2 Colour roles and the state-layer helper

```rust
// crates/ns_app/src/ui/theme.rs
use egui::Color32;
pub const SURFACE_LOWEST: Color32 = Color32::from_rgb(0x08, 0x0F, 0x12);
pub const SURFACE:        Color32 = Color32::from_rgb(0x0E, 0x14, 0x17);
pub const CONTAINER_LOW:  Color32 = Color32::from_rgb(0x16, 0x1D, 0x1F);
pub const CONTAINER:      Color32 = Color32::from_rgb(0x1A, 0x21, 0x23);
pub const CONTAINER_HIGH: Color32 = Color32::from_rgb(0x24, 0x2B, 0x2E);
pub const CONTAINER_HIGHEST: Color32 = Color32::from_rgb(0x2F, 0x36, 0x39);
pub const ON_SURFACE:     Color32 = Color32::from_rgb(0xDD, 0xE3, 0xE7);
pub const ON_SURFACE_VAR: Color32 = Color32::from_rgb(0xBB, 0xC9, 0xCE);
pub const OUTLINE:        Color32 = Color32::from_rgb(0x85, 0x93, 0x98);
pub const OUTLINE_VARIANT:Color32 = Color32::from_rgb(0x3C, 0x49, 0x4D);
pub const PRIMARY:        Color32 = Color32::from_rgb(0x00, 0xD6, 0xFC);
pub const ON_PRIMARY:     Color32 = Color32::from_rgb(0x00, 0x36, 0x41);
pub const SECONDARY_CONTAINER:    Color32 = Color32::from_rgb(0x13, 0x4D, 0x5B);
pub const ON_SECONDARY_CONTAINER: Color32 = Color32::from_rgb(0x8A, 0xBD, 0xCD);
pub const ERROR:          Color32 = Color32::from_rgb(0xFF, 0xB4, 0xAB);
pub const WARN:           Color32 = Color32::from_rgb(0xF2, 0xC1, 0x4E);
pub const OK:             Color32 = Color32::from_rgb(0x2F, 0xD2, 0x7A);
```

```rust
// theme/state.rs
use egui::Color32;
pub const HOVER: f32 = 0.08;
pub const FOCUS: f32 = 0.10;
pub const PRESS: f32 = 0.10;
pub const DRAG:  f32 = 0.16;

/// Opaque blend of `content` over `base` at `alpha` (an M3 state layer).
pub fn layer(base: Color32, content: Color32, alpha: f32) -> Color32 {
    let m = |b: u8, c: u8| (b as f32 + (c as f32 - b as f32) * alpha).round() as u8;
    Color32::from_rgb(m(base.r(), content.r()), m(base.g(), content.g()), m(base.b(), content.b()))
}
```

### 6.3 Global theme (the biggest win for the least code)

```rust
// theme/mod.rs
use egui::{
    epaint::Shadow,
    style::{HandleShape, Selection, WidgetVisuals},
    vec2, Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle,
};
pub mod colors; pub mod state; pub mod tokens; pub mod fonts;
use colors::*; use state::*;

pub fn apply(ctx: &egui::Context) {
    let mut s = (*ctx.style()).clone();
    let r = CornerRadius::same(8);

    // A widget look with NO stroke. Fill and text colour only.
    let wv = |fill: Color32, fg: Color32| WidgetVisuals {
        bg_fill: fill,
        weak_bg_fill: fill,
        bg_stroke: Stroke::NONE,
        corner_radius: r,
        fg_stroke: Stroke::new(1.5, fg),
        expansion: 0.0,
    };

    let v = &mut s.visuals;
    v.dark_mode = true;
    v.override_text_color = None;
    v.panel_fill = SURFACE;                  // shows in the gaps between panels
    v.window_fill = CONTAINER_HIGH;
    v.window_stroke = Stroke::NONE;          // floating things use shadow, not lines
    v.window_corner_radius = CornerRadius::same(16);
    v.menu_corner_radius = CornerRadius::same(8);
    v.window_shadow = Shadow { offset: [0, 8], blur: 24, spread: 0, color: Color32::from_black_alpha(100) };
    v.popup_shadow  = Shadow { offset: [0, 4], blur: 12, spread: 0, color: Color32::from_black_alpha(90) };
    v.extreme_bg_color = CONTAINER_HIGHEST;  // TextEdit background
    v.faint_bg_color = CONTAINER;            // striped rows
    v.slider_trailing_fill = true;
    v.handle_shape = HandleShape::Circle;
    v.indent_has_left_vline = false;         // removes the vertical line in nested collapsing headers
    v.selection = Selection { bg_fill: PRIMARY, stroke: Stroke::new(1.0, ON_PRIMARY) };

    // noninteractive = labels and separators. Its bg_stroke is what ui.separator() draws.
    v.widgets.noninteractive = WidgetVisuals {
        bg_stroke: Stroke::new(1.0, OUTLINE_VARIANT),
        ..wv(CONTAINER_LOW, ON_SURFACE)
    };
    // Stock buttons, DragValue, ComboBox: neutral filled, no stroke.
    v.widgets.inactive = wv(CONTAINER_HIGHEST, ON_SURFACE_VAR);
    v.widgets.hovered  = wv(layer(CONTAINER_HIGHEST, ON_SURFACE, HOVER), ON_SURFACE);
    v.widgets.active   = wv(layer(CONTAINER_HIGHEST, ON_SURFACE, PRESS), ON_SURFACE);
    v.widgets.open     = wv(layer(CONTAINER_HIGHEST, ON_SURFACE, FOCUS), ON_SURFACE);

    // Spacing: compact desktop density.
    let sp = &mut s.spacing;
    sp.item_spacing = vec2(8.0, 8.0);
    sp.button_padding = vec2(12.0, 6.0);
    sp.interact_size = vec2(40.0, 32.0);
    sp.window_margin = egui::Margin::same(16);
    sp.menu_margin = egui::Margin::same(8);
    sp.indent = 16.0;
    sp.scroll = egui::style::ScrollStyle::floating(); // thin overlay scrollbars, no track
    s.interaction.tooltip_delay = 0.4;

    // Type scale.
    use TextStyle::*;
    s.text_styles = [
        (Heading,   FontId::new(16.0, FontFamily::Name("medium".into()))),
        (Body,      FontId::new(14.0, FontFamily::Proportional)),
        (Button,    FontId::new(13.0, FontFamily::Name("medium".into()))),
        (Small,     FontId::new(12.0, FontFamily::Proportional)),
        (Monospace, FontId::new(13.0, FontFamily::Monospace)),
    ].into();

    ctx.set_style(s);
}
```

Notes on this block:
- `ui.separator()`, `Frame::group`, `ui.group()` and egui's default panel frame all read `noninteractive.bg_stroke`. Replace them with space or custom frames (section 6.5) so the stroke above is rarely used.
- `ComboBox`, `DragValue` and stock `ui.button` share `weak_bg_fill`, so they will look like neutral filled fields. Accent and tonal buttons are custom (section 6.4).
- `SelectableLabel` and text selection use `selection.bg_fill` (solid primary). Use the custom chip, rail item and layer row instead of `selectable_label` for selected-state UI.
- If the stock `Slider` handle or track colours don't match, start with `slider_trailing_fill` and widget fills as above, then write a custom slider if needed. Check this in the style editor.

### 6.4 Fonts and icons

```rust
// theme/fonts.rs
use egui::{FontData, FontDefinitions, FontFamily};
pub fn install(ctx: &egui::Context) {
    let mut f = FontDefinitions::default();
    f.font_data.insert("plex".into(),        FontData::from_static(include_bytes!("../../../assets/IBMPlexSans-Regular.ttf")).into());
    f.font_data.insert("plex-medium".into(), FontData::from_static(include_bytes!("../../../assets/IBMPlexSans-Medium.ttf")).into());
    f.font_data.insert("icons".into(),       FontData::from_static(include_bytes!("../../../assets/MaterialSymbolsRounded.ttf")).into());
    f.families.entry(FontFamily::Proportional).or_default().insert(0, "plex".into());
    f.families.insert(FontFamily::Name("medium".into()), vec!["plex-medium".into(), "plex".into()]);
    f.families.entry(FontFamily::Proportional).or_default().push("icons".into()); // icon glyphs as fallback
    ctx.set_fonts(f);
}
```
- Use **one icon set**: Material Symbols (Rounded or Outlined, 20 px, weight 400). Check the font's licence terms for your distribution. Define icon constants as `const ICON_VISIBILITY: &str = "\u{e8f4}";` (verify code points against the font file you ship).
- `FontData` and the `font_data` map type changed across versions (`Arc<FontData>` in recent ones). The `.into()` above covers it. Adjust if your version differs.
- Subset the icon font to the glyphs you use if file size matters.

### 6.5 Panels as rounded surfaces (no lines)

```rust
// panels/mod.rs
use egui::{Frame, Margin};
use crate::ui::theme::colors::*;

pub fn panel() -> Frame {
    Frame::new().fill(CONTAINER_LOW).corner_radius(12)
        .inner_margin(Margin::same(12)).outer_margin(Margin::same(4))
}
pub fn group() -> Frame {                       // replaces ui.group()
    Frame::new().fill(CONTAINER).corner_radius(12).inner_margin(Margin::same(12))
}
pub fn viewport() -> Frame {
    Frame::new().fill(SURFACE_LOWEST).corner_radius(12).outer_margin(Margin::same(4))
}
```

```rust
// in update()
egui::SidePanel::left("rail").exact_width(72.0).frame(panel()).show_separator_line(false).show(ctx, |ui| nav_rail(ui, app));
egui::SidePanel::left("layers").default_width(296.0).width_range(240.0..=420.0)
    .frame(panel()).show_separator_line(false).show(ctx, |ui| layers_and_export(ui, app));
egui::SidePanel::right("props").default_width(332.0).width_range(280.0..=480.0)
    .frame(panel()).show_separator_line(false).show(ctx, |ui| properties(ui, app));
egui::TopBottomPanel::bottom("status").frame(Frame::new().fill(SURFACE).inner_margin(egui::Margin::symmetric(12, 4)))
    .show_separator_line(false).show(ctx, |ui| status_bar(ui, app));
egui::CentralPanel::default().frame(viewport()).show(ctx, |ui| viewport_ui(ui, app));
```
Panel type names and `show_separator_line` have moved between releases. Use what your version exposes. Keep resizing: egui's resize handle only appears on hover, which is what you want.

### 6.6 Custom widgets (egui has no M3 widgets, so write these once)

```rust
// widgets/button.rs
use egui::{vec2, Align2, Color32, CornerRadius, FontId, Response, Sense, Stroke, StrokeKind, Ui, WidgetInfo, WidgetType};
use crate::ui::theme::{colors::*, state::*};

#[derive(Clone, Copy, PartialEq)]
pub enum Kind { Filled, Tonal, Text }

pub fn button(ui: &mut Ui, label: &str, kind: Kind) -> Response {
    let font = FontId::new(13.0, egui::FontFamily::Name("medium".into()));
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, Color32::WHITE);
    let size = vec2((galley.size().x + 24.0).max(64.0), 32.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let enabled = ui.is_enabled();
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, label));

    if ui.is_rect_visible(rect) {
        let (bg, fg) = match kind {
            Kind::Filled => (PRIMARY, ON_PRIMARY),
            Kind::Tonal  => (SECONDARY_CONTAINER, ON_SECONDARY_CONTAINER),
            Kind::Text   => (Color32::TRANSPARENT, PRIMARY),
        };
        let cr = CornerRadius::same(8);
        let p = ui.painter();
        if !enabled {
            p.rect_filled(rect, cr, ON_SURFACE.gamma_multiply(0.12));
            p.text(rect.center(), Align2::CENTER_CENTER, label, FontId::proportional(13.0), ON_SURFACE.gamma_multiply(0.38));
            return resp;
        }
        let t = ui.ctx().animate_bool_with_time(resp.id.with("hover"), resp.hovered(), 0.12);
        let a = if resp.is_pointer_button_down_on() { PRESS } else if resp.has_focus() { FOCUS } else { HOVER * t };
        p.rect_filled(rect, cr, bg);
        p.rect_filled(rect, cr, fg.gamma_multiply(a));            // state layer
        p.galley(rect.center() - galley.size() / 2.0, galley, fg);
        if resp.has_focus() {                                      // keyboard focus ring
            p.rect_stroke(rect.expand(2.0), CornerRadius::same(10), Stroke::new(2.0, PRIMARY), StrokeKind::Outside);
        }
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}
```
Notes: the galley is laid out with a white placeholder colour and recoloured by `p.galley(pos, galley, fg)`. Check the exact signature in your version (it changed). Animation only requests repaints while it is changing, which keeps egui's reactive mode cheap.

**Filter chip (export maps):**
- Height 32, radius 8, horizontal padding 12.
- Off: `CONTAINER_HIGH` fill, `ON_SURFACE_VAR` text.
- On: `SECONDARY_CONTAINER` fill, `ON_SECONDARY_CONTAINER` text, leading check icon (18 px). Layout shifts only by the icon width.
- Same state layers as the button; click toggles a `&mut bool`. Add `WidgetInfo::selected(WidgetType::Checkbox, enabled, on, label)` for screen readers.

**Segmented button (Normal Y, 8/16-bit):** one rounded `CONTAINER_HIGH` container; each option is an equal-width cell; selected cell is `SECONDARY_CONTAINER` with radius 8 inside the container (inset 2 px). No outline, no dividers.

**Switch:** 40×22 track, thumb 14 px off / 18 px on; off track `CONTAINER_HIGHEST` with `OUTLINE` thumb, on track `PRIMARY` with `ON_PRIMARY` thumb; animate the thumb position with `animate_bool_with_time(..., 0.15)`.

**Nav rail item:** 64×56 hit area: icon (24 px) on top, label (12 px) below. Selected: a 40×28 `SECONDARY_CONTAINER` pill behind the icon (animate width from 0 for a small M3 touch), icon and label `ON_SECONDARY_CONTAINER`. Hover: 8% layer on the pill area. Shortcut letter shown in the tooltip, not as a corner badge.

**Filled text field:** wrap `TextEdit::singleline(..).frame(false)` in a `Frame` with `CONTAINER_HIGHEST` fill and radius 8 (top corners only if you want the M3 filled look). Paint a 1 px `ON_SURFACE_VAR` bottom line at rest if the field needs a clearer affordance, 2 px `PRIMARY` when focused, 1 px `ERROR` when invalid.

**Layer row (custom painted, not `selectable_label`):**
```text
| ⠿ (on hover) | 👁 32 | thumb 40 | name + type | blend badge | 🔒 32 |    row 52 px, radius 12
```
- Rest: transparent. Hover: 8% `ON_SURFACE`. Selected: `SECONDARY_CONTAINER` fill with `ON_SECONDARY_CONTAINER` text. Dragging: 16% layer plus elevation 2 shadow on the dragged ghost.
- Hidden layer: name and thumb at 45% opacity (`gamma_multiply(0.45)`).
- Blend badge: no outline; `CONTAINER_HIGHEST` tile, 12 px text.

**Section group in Properties:** `group()` frame, title in `Heading` style with chevron, content separated by 12 px gaps. No separator lines inside.

### 6.7 Slider rows (Properties)
```text
Label (92 px, on_surface_variant) | slider (flex) | value field (64 px, filled)
```
- Active track `PRIMARY`, inactive `CONTAINER_HIGHEST`, 4 px thick, round thumb 16 px (20 px while dragging, 16% state layer behind it).
- Value field is the filled text field with the unit shown after the number in `ON_SURFACE_VAR`.
- Double-click label or thumb resets to default. Arrow keys step; Shift = ×10.

### 6.8 Tabs (Properties / Mesh / Bake / Lightmap / PBR)
- Row height 40. Labels `label` style. Active: `ON_SURFACE` text + 3 px `PRIMARY` bar (rounded top) under the label. Inactive: `ON_SURFACE_VAR`.
- Hover/press: 8%/10% layer over the tab cell. Keyboard: ←/→, Home/End, Ctrl+1…5.
- Optional single `OUTLINE_VARIANT` divider under the row.

### 6.9 Export button states
- Idle: Filled "Export 2 maps".
- Disabled (0 maps): disabled style plus a caption saying why.
- Running: same button with a progress fill (`ON_PRIMARY` at 20%) and a Text-variant "Cancel" next to it.
- Done: toast (section 7) with a Text-variant "Open folder".

---

## 7. Feedback, motion, overlays

| Thing | Spec |
|---|---|
| Hover/press layers | 120 ms ease-out via `animate_bool_with_time` |
| Switch / chip / indicator | 150–200 ms |
| Panel collapse | 200 ms (`CollapsingHeader` animates by default) |
| Toast | `Area` anchored bottom centre above the status bar; fill inverse surface (`#E6E8EE` on `#12151A` text), elevation 2, radius 8; 6 s auto-dismiss, pauses on hover; optional Text action (Undo, Open folder) |
| Tooltip | 400 ms delay (`interaction.tooltip_delay`), `radius_xs`, inverse surface, text `Name (Key)` |
| Menus (File/Edit/View/Tools, combo popups) | `CONTAINER_HIGH`, radius 8, elevation 2, items 32 px with 8% hover layer |
| Memory warning | Status bar text in `WARN` at ≥ 85%, `ERROR` at ≥ 95%, always with icon and advice |
| Reduce motion | One app setting. When on, set all animation times to 0 (route every animation call through one helper) |

**Repaint discipline:** egui is reactive, so animations must only request repaints while a value is changing (`animate_*` does this). Never call `ctx.request_repaint()` unconditionally from UI code, and keep the 3D viewport on its own paint callback so UI animation does not force expensive re-renders.

---

## 8. Keeping controls identifiable without borders (accessibility)

WCAG 1.4.11 asks that the visual information needed to identify a control is distinguishable. Borderless is fine **if** identification does not rely on a low-contrast fill edge. Use these rules:

1. **Buttons** with a text label are identified by that label's contrast (≥ 4.5:1). Filled and tonal variants add a fill on top.
2. **Fields without a visible label inside** need a second cue: bottom indicator line, plus a 8% hover layer and a primary focus indicator.
3. **Sliders:** track (inactive part) must be at least 3:1 against the panel, or the thumb must be (round, 16 px, `ON_SURFACE`).
4. **Switches and chips:** state is shown by fill *and* shape/icon (thumb position, ✓), never colour alone.
5. **Container fill vs panel** (for example `CONTAINER_HIGHEST` on `CONTAINER_LOW`) is about 1.3:1. That is fine for decoration but must never be the only identifier of a control.
6. **Focus ring** is the one outline that must stay: 2 px `PRIMARY`, ≥ 3:1 against its surroundings, visible for keyboard focus (WCAG 2.4.7).
7. egui integrates AccessKit. Add `widget_info` to every custom widget so screen readers get a name, role and state.
8. Test in Windows High Contrast / forced colours if you ship to users who rely on it, and consider a "High contrast" theme that turns resting borders back on (a `bool` in the theme that adds `Stroke::new(1.0, OUTLINE)` to `wv`).

---

## 9. Border audit (do this before and after)

Search the codebase and justify every hit against the budget in section 5:

```bash
grep -rnE "Stroke::new|bg_stroke|window_stroke|\.stroke\(|rect_stroke|ui\.separator|ui\.group|Frame::group|show_separator_line\(true\)|StrokeKind" src/
```

egui defaults that draw lines, and the fix:

| Source of lines | Fix |
|---|---|
| `ui.group()`, `Frame::group` | `panels::group()` (filled, no stroke) |
| `ui.separator()` | `ui.add_space(16.0)`; keep at most one per panel |
| Panel default frames and resize lines | Custom `Frame` + `show_separator_line(false)` |
| `Window` / popup stroke | `window_stroke = Stroke::NONE`, shadow instead |
| `ComboBox`, `DragValue`, `Button` outline | `widgets.*.bg_stroke = Stroke::NONE` (done in `apply`) |
| `TextEdit` frame | `.frame(false)` inside a custom filled frame |
| Nested `CollapsingHeader` vertical line | `indent_has_left_vline = false` |
| Scroll area track | `ScrollStyle::floating()` |
| Table/Grid striping and lines | `striped(true)` with `faint_bg_color`, no lines |

**Success metric:** take a resting-state screenshot with no hover or focus. There should be no visible outlines except checkbox/radio boxes. Compare with the current screenshot.

---

## 10. Component map (current → Material → egui)

| N-shade element | Material component | egui approach |
|---|---|---|
| Left tool rail | Navigation rail | Custom `nav_rail` widget |
| Layers list | List with selected state | Custom `layer_row` painter |
| "+ Add layer" | Filled tonal button (with icon) | Custom `button(Kind::Tonal)` |
| Layer footer icons | Icon buttons | Custom icon button, 32 px, 8% layer |
| Reset layers | Text button (danger on hover) | `button(Kind::Text)` |
| Export maps checklist | Filter chips | Custom `chip` |
| 8/16-bit, OpenGL/DirectX | Segmented buttons | Custom `segmented` |
| Size / Height source | Filled select | `ComboBox` styled by `apply` |
| Save to / Name | Filled text field | `TextEdit` in filled frame |
| Export maps | Filled button | `button(Kind::Filled)` |
| Visible, Carve, Include project | Switch / checkbox | Custom `switch`; stock `Checkbox` for plain lists |
| Radius/Height/Softness/Smooth | Slider + value field | Styled `Slider` or custom |
| Properties tabs | Primary tabs | Custom tab row |
| Mask / Shape sections | Cards (filled) | `panels::group()` + `CollapsingHeader` |
| Viewport tabs (3D/2D/Split) | Secondary tabs or segmented | Custom tab row, compact |
| Status bar | Bottom bar | `TopBottomPanel::bottom` with filled frame |
| Undo toast, export toast | Snackbar | `Area` + custom frame |
| Menus | Menus | `ui.menu_button` styled by `apply` |

---

## 11. Implementation order

1. **Phase A, theme (half a day, biggest visible change):** `fonts::install`, `theme::apply`, remove stray `ui.separator()`/`ui.group()` calls. Everything already on screen loses its outlines and gets the new surfaces.
2. **Phase B, panels:** rounded panel frames, gaps, `show_separator_line(false)`, viewport container.
3. **Phase C, widgets:** `button`, `chip`, `segmented`, `switch`, `field`; swap them into Export and Properties.
4. **Phase D, rail and layers:** `nav_rail`, `layer_row`, lock toggle, keyboard reorder.
5. **Phase E, tabs, toasts, menus, tooltips, memory warning.**
6. **Phase F, audit:** run the section 9 grep and screenshot comparison; fix leftovers; add the High contrast option.

After each phase, run the regression checklist in `UI_IMPROVEMENT.md` section 9. Export output must remain byte-identical, because none of this touches the data model or shaders.

### IDE prompt for this addendum
```text
Read UI_IMPROVEMENT.md and UI_IMPROVEMENT_MATERIAL_EGUI.md. Implement Phase <X> of the addendum in Rust/egui.
Rules:
- Presentation only. Do not change data models, export code, shaders or file formats. Keep every feature in UI_IMPROVEMENT.md section 1.
- No resting-state borders. Follow the border budget (addendum section 5). Run the section 9 grep and explain each remaining hit.
- Use only the colour roles in theme/colors.rs. No hard-coded colours in panel code.
- State layers: hover 8%, focus 10%, pressed 10%, dragged 16%.
- Every custom widget needs widget_info, a 24 px minimum hit area (32 preferred), a keyboard focus ring and a tooltip with shortcut.
- Match my pinned egui version's API. If a name differs from the document, adapt it and tell me what you changed.
- Request repaints only while animating.
```

---

## 12. Open questions

1. Which egui/eframe version is pinned, and do you use `egui_wgpu` callbacks for the 3D viewport?
2. What icon set do you use now? Moving to one set (Material Symbols or Phosphor) is part of Phase A.
3. Which font ships today? The tokens assume IBM Plex Sans Regular + Medium.
4. Do you want a light theme and a high-contrast theme, or dark only?
5. Are any custom widgets already painted by hand (curve editor, layer thumbnails)? They need the same state-layer and focus rules.

---

## 13. Sources and confidence

- **egui API shapes** (`Visuals`, `Widgets`/`WidgetVisuals`, `CornerRadius`, `Style`): from docs.rs listings for egui 0.34.x and from open-source egui themes that use the same fields. Treat field names as likely correct but verify on your version.
- **Material 3 state-layer values** (hover 8%, focus 10%, pressed 10%, dragged 16%), tonal surfaces, elevation levels and the button variants (filled, tonal, outlined, text, elevated): from third-party Material 3 implementations and token packages, not from the official Material site directly. They match the M3 spec as I know it, but check the official Material 3 guidelines for any value you want to cite formally.
- The colour hex values and contrast ratios are **my estimates**. Measure them on real pixels.

---

## Implementation status (2026-10-28)

Done: Phase A (colour roles, state layers, borderless `theme::apply`), Phase B (rounded panel surfaces with 8 px gaps, viewport well, no panel separators), Phase C (filled button, filter chips, segmented controls, M3 switch, checkbox and slider, filled text fields via the stock widgets), Phase D (labelled navigation rail with the M3 selected pill, tonal layer rows, dashed empty mask slot, 16% drag ghost), Phase E (primary tabs, inverse-surface snackbar, menu/popup shadows, memory warning roles).

Deviations:
- Tooltips stay on `surface_container_high`; egui shares `window_fill` between tooltips and floating windows, so an inverse tooltip would also invert dialogs.
- Text fields are filled `surface_container_highest` with no resting underline; egui's `TextEdit` has no focus indicator hook, and a per-field wrapper would touch every form.
- The tool rail uses 48 px compact items (not 56) so nine tools plus the 3D/2D switch fit a 720 px-high window.
- Keyboard focus rings are painted by the custom widgets; stock `Button`/`ComboBox` do not expose a focus hook.
- No reduce-motion or high-contrast switch yet.

Icons: the supplied `assets/icons` SVG pack is embedded with `egui::include_image!` and rendered through `egui_extras`' SVG loader. Every tool-rail tile now uses the pack, including the added ruler, node, add/delete anchor and convert-point glyphs; layer rows use eye/eye-off; the footer uses up/down/brush/trash. The small flyout menus keep the hand-drawn variants where 14 px detail reads better.

Not done: layer lock/solo and Alt+Arrow reorder (these need data-model changes, not presentation), export progress/cancel (export is synchronous).
