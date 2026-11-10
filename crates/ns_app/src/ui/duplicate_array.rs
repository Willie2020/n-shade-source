//! Duplicate-array dialog: repeat the selection at a precise offset.
//!
//! The dialog is intentionally small: a count and an X/Y step. The first copy
//! is created from the selection, then each following copy is created from the
//! previous one, so every copy is one step further along the same direction.
//! The whole operation is a single undo entry.

use std::collections::HashSet;

use super::*;

/// Floating dialog and its apply step.
pub(crate) fn duplicate_array_window(
    ctx: &egui::Context,
    state: &mut EditorState,
    doc: &mut DocumentRes,
) {
    let mut open = state.show_duplicate_array;
    let mut apply = false;
    let mut cancel = false;
    egui::Window::new("Duplicate array")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(280.0)
        .show(ctx, |ui| {
            ui.label(
                egui::RichText::new("Repeat the selected layers with a precise offset.")
                    .size(TEXT_SMALL)
                    .color(MUTED),
            );
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("Count");
                ui.add(
                    egui::DragValue::new(&mut state.duplicate_array_count)
                        .range(2..=64)
                        .speed(1.0),
                );
                ui.label(
                    egui::RichText::new("(includes the original)")
                        .size(TEXT_CAPTION)
                        .color(MUTED),
                );
            });
            ui.horizontal(|ui| {
                ui.label("Offset X");
                ui.add(
                    egui::DragValue::new(&mut state.duplicate_array_offset.x)
                        .range(-4096.0..=4096.0)
                        .speed(1.0)
                        .max_decimals(1),
                );
                ui.label("Y");
                ui.add(
                    egui::DragValue::new(&mut state.duplicate_array_offset.y)
                        .range(-4096.0..=4096.0)
                        .speed(1.0)
                        .max_decimals(1),
                );
            });
            let zero = state.duplicate_array_offset.length_squared() < 1e-4;
            if zero {
                ui.label(
                    egui::RichText::new("Set a non-zero offset.")
                        .size(TEXT_CAPTION)
                        .color(WARNING),
                );
            }
            ui.label(
                egui::RichText::new("Each copy is one offset further than the previous one.")
                    .size(TEXT_CAPTION)
                    .color(MUTED),
            );
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!zero, egui::Button::new("Duplicate"))
                    .clicked()
                {
                    apply = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
        });
    if apply {
        apply_duplicate_array(state, doc);
        open = false;
    }
    if cancel {
        open = false;
    }
    state.show_duplicate_array = open;
}

/// Create `count - 1` copies, each one offset step further than the previous.
/// Returns the number of copies, or 0 when nothing could be duplicated.
pub(crate) fn apply_duplicate_array(state: &mut EditorState, doc: &mut DocumentRes) -> usize {
    let count = state.duplicate_array_count.clamp(2, 64) as usize;
    let offset = state.duplicate_array_offset;
    if offset.length_squared() < 1e-4 {
        state.status = "Duplicate array: set a non-zero offset".into();
        return 0;
    }

    let mut source_paths: Vec<Vec<usize>> = Vec::new();
    if let Some(path) = state.selected.clone() {
        source_paths.push(path);
    }
    source_paths.extend(state.multi_selected.iter().cloned());
    if source_paths.is_empty() {
        state.status = "Duplicate array: select a layer first".into();
        return 0;
    }

    state.checkpoint_document(&doc.0);
    let mut created_ids: Vec<LayerId> = Vec::new();
    let mut moved_shapes = 0usize;
    for _ in 1..count {
        let copies = duplicate_layers(state, doc, &source_paths);
        if copies.is_empty() {
            break;
        }
        for path in &copies {
            if let Some(layer) = doc.0.layer_by_path(path) {
                created_ids.push(layer.id);
            }
        }
        moved_shapes += translate_paths(doc, &copies, offset);
        source_paths = copies;
    }

    if created_ids.is_empty() {
        state.status = "Duplicate array: nothing to copy".into();
        return 0;
    }

    let paths: Vec<Vec<usize>> = created_ids
        .iter()
        .filter_map(|id| doc.0.find_path(*id))
        .collect();
    if let Some(primary) = paths.first().cloned() {
        state.selected = Some(primary);
        state.multi_selected = paths.into_iter().skip(1).collect();
    }
    state.mark_dirty();

    let copies = count - 1;
    state.status = if moved_shapes > 0 {
        format!(
            "Array duplicated · {copies} copies at ({:.1}, {:.1}) px",
            offset.x, offset.y
        )
    } else {
        format!("Array duplicated · {copies} copies (no outline to offset)")
    };
    copies
}

/// Shift every shape under the copied paths by `delta`.
fn translate_paths(doc: &mut DocumentRes, paths: &[Vec<usize>], delta: Vec2) -> usize {
    let mut ids = Vec::new();
    for path in paths {
        if let Some(layer) = doc.0.layer_by_path(path) {
            collect_shape_ids(layer, &mut ids);
        }
    }
    let mut seen = HashSet::new();
    ids.retain(|id| seen.insert(*id));

    let mut moved = 0;
    for id in ids {
        let Some(layer) = doc.0.find_layer_mut(id) else {
            continue;
        };
        let LayerKind::Shape(shape) = &mut layer.kind else {
            continue;
        };
        if let Some(source) = shape.source.transformed(Vec2::ONE, delta) {
            shape.source = source;
            layer.touch();
            moved += 1;
        }
    }
    moved
}

fn collect_shape_ids(layer: &Layer, out: &mut Vec<LayerId>) {
    match &layer.kind {
        LayerKind::Shape(_) => out.push(layer.id),
        LayerKind::Group { children } => {
            for child in children {
                collect_shape_ids(child, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::UVec2;
    use ns_core::{Document, Primitive, ShapeSource};

    fn rect(name: &str, center: Vec2) -> Layer {
        Layer::shape(
            name,
            ShapeSource::Primitive(Primitive::Rect {
                center,
                size: Vec2::splat(20.0),
                corner_radius: 0.0,
            }),
        )
    }

    fn centre(layer: &Layer) -> Vec2 {
        match &layer.kind {
            LayerKind::Shape(shape) => shape.source.bounds().unwrap().center(),
            LayerKind::Group { children } => {
                children
                    .iter()
                    .map(centre)
                    .fold(Vec2::ZERO, |sum, point| sum + point)
                    / children.len() as f32
            }
            _ => Vec2::ZERO,
        }
    }

    #[test]
    fn duplicates_a_row_at_the_requested_spacing() {
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(240)));
        doc.0.layers.push(rect("panel", Vec2::new(40.0, 40.0)));
        let mut state = EditorState {
            selected: Some(vec![0]),
            duplicate_array_count: 3,
            duplicate_array_offset: Vec2::new(40.0, 0.0),
            ..Default::default()
        };

        assert_eq!(apply_duplicate_array(&mut state, &mut doc), 2);
        assert_eq!(doc.0.layers.len(), 3);
        assert!((centre(&doc.0.layers[0]) - Vec2::new(40.0, 40.0)).length() < 1e-4);
        assert!((centre(&doc.0.layers[1]) - Vec2::new(80.0, 40.0)).length() < 1e-4);
        assert!((centre(&doc.0.layers[2]) - Vec2::new(120.0, 40.0)).length() < 1e-4);
        assert_eq!(state.undo.len(), 1, "the array is one undo step");
        assert_eq!(state.selected, Some(vec![1]));
        assert_eq!(state.multi_selected, vec![vec![2]]);

        assert!(state.undo(&mut doc));
        assert_eq!(doc.0.layers.len(), 1, "undo removes the whole array");
    }

    #[test]
    fn duplicates_a_group_and_moves_its_children() {
        let first = rect("a", Vec2::new(30.0, 30.0));
        let second = rect("b", Vec2::new(60.0, 30.0));
        let group = Layer::group("group", vec![first, second]);
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(240)));
        doc.0.layers.push(group);
        let mut state = EditorState {
            selected: Some(vec![0]),
            duplicate_array_count: 2,
            duplicate_array_offset: Vec2::new(0.0, 40.0),
            ..Default::default()
        };

        assert_eq!(apply_duplicate_array(&mut state, &mut doc), 1);
        assert_eq!(doc.0.layers.len(), 2);
        let copy = &doc.0.layers[1];
        assert!(copy.is_group());
        assert!((centre(copy) - Vec2::new(45.0, 70.0)).length() < 1e-4);
    }

    #[test]
    fn duplicates_a_multi_selection_at_one_offset() {
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(240)));
        doc.0.layers.push(rect("a", Vec2::new(40.0, 30.0)));
        doc.0.layers.push(rect("b", Vec2::new(120.0, 30.0)));
        let mut state = EditorState {
            selected: Some(vec![0]),
            multi_selected: vec![vec![1]],
            duplicate_array_count: 2,
            duplicate_array_offset: Vec2::new(0.0, 50.0),
            ..Default::default()
        };

        assert_eq!(apply_duplicate_array(&mut state, &mut doc), 1);
        assert_eq!(doc.0.layers.len(), 4);
        let centres: Vec<Vec2> = doc.0.layers.iter().map(centre).collect();
        assert!((centres[0] - Vec2::new(40.0, 30.0)).length() < 1e-4);
        assert!((centres[1] - Vec2::new(40.0, 80.0)).length() < 1e-4);
        assert!((centres[2] - Vec2::new(120.0, 30.0)).length() < 1e-4);
        assert!((centres[3] - Vec2::new(120.0, 80.0)).length() < 1e-4);
    }

    #[test]
    fn a_zero_offset_does_nothing() {
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(120)));
        doc.0.layers.push(rect("panel", Vec2::splat(30.0)));
        let mut state = EditorState {
            selected: Some(vec![0]),
            duplicate_array_count: 4,
            duplicate_array_offset: Vec2::ZERO,
            ..Default::default()
        };
        assert_eq!(apply_duplicate_array(&mut state, &mut doc), 0);
        assert_eq!(doc.0.layers.len(), 1);
    }
}
