# N-shade icon set

> **Integration note:** these SVGs are embedded at compile time with
> `egui::include_image!` and rendered by `egui_extras`' SVG loader (0.31, `svg`
> feature). Every tool-rail tile uses the pack (including the added ruler,
> node, add/delete anchor and convert-point glyphs). The layer eye and layer
> footer also use it. Small flyout menus keep the hand-drawn variants where
> 14 px detail reads better.

SVG files are drawn with a white stroke on a 24x24 grid so egui can tint them at draw time.
The downloaded vector-tool pack uses `stroke="currentColor"`; it is stored here as
`#fff` because egui's SVG loader resolves `currentColor` to black and tinting
multiplies the rasterised pixels.

## egui setup
```toml
egui_extras = { version = "0.34", features = ["svg"] }
```
```rust
egui_extras::install_image_loaders(&ctx);          // once at startup
const PEN: egui::ImageSource = egui::include_image!("../assets/icons/pen.svg");
```

## Rail button (pill selected state, flyout triangle)
```rust
pub fn tool_button(ui: &mut egui::Ui, src: egui::ImageSource<'_>, label: &str, selected: bool, flyout: bool) -> egui::Response {
    use egui::{vec2, Rect, Sense, Align2, FontId, Shape, Stroke, pos2};
    use crate::ui::theme::{colors::*, state::*};
    let (rect, resp) = ui.allocate_exact_size(vec2(64.0, 52.0), Sense::click());
    let pill = Rect::from_center_size(rect.center_top() + vec2(0.0, 18.0), vec2(48.0, 30.0));
    let (fg, bg) = if selected { (ON_SECONDARY_CONTAINER, SECONDARY_CONTAINER) } else { (ON_SURFACE_VAR, egui::Color32::TRANSPARENT) };
    let a = if resp.is_pointer_button_down_on() { PRESS } else if resp.hovered() { HOVER } else { 0.0 };
    let p = ui.painter();
    p.rect_filled(pill, 15.0, bg);
    p.rect_filled(pill, 15.0, ON_SURFACE.gamma_multiply(a));       // state layer
    egui::Image::new(src).tint(fg).paint_at(ui, Rect::from_center_size(pill.center(), vec2(22.0, 22.0)));
    if flyout {
        let c = pill.right_bottom() - vec2(7.0, 5.0);
        p.add(Shape::convex_polygon(vec![c, c + vec2(-6.0, 0.0), c + vec2(0.0, -6.0)], fg, Stroke::NONE));
    }
    p.text(pos2(rect.center().x, rect.bottom() - 6.0), Align2::CENTER_CENTER, label, FontId::proportional(11.0), fg);
    if resp.has_focus() { p.rect_stroke(pill.expand(2.0), 17.0, Stroke::new(2.0, PRIMARY), egui::StrokeKind::Outside); }
    resp.on_hover_text(label)
}
```
Check names (`paint_at`, `rect_filled` radius type, `rect_stroke` signature) against your pinned egui version.

## Notes
- SVGs are rasterised at the size you draw them, so they stay sharp at any `pixels_per_point`.
- Group order used in the preview: Select, Pan | Rectangle, Ellipse, Star, Pen | Lasso, Zoom | Brush, Paint, Eraser.
- Tool-rail glyphs are drawn at 26 px inside the 40 × 26 px selected pill, so the
  artwork's own viewBox inset is the only visible margin.
