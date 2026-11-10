//! Layer-stack insertion, grouping, deletion, and reordering.

use super::*;

use std::collections::HashMap;

pub(crate) enum LayerAction {
    AddRect,
    AddEllipse,
    AddStar,
    NewGroup,
    GroupSelected,
    UngroupSelected,
    Delete,
    /// Freeze or unfreeze the selected layers' content.
    ToggleLock,
    /// Deep-copy the selection directly above the originals.
    Duplicate,
    /// Bind the layer below as this layer's mask (or unbind it).
    ToggleMask,
    /// Bind the selected layer to the layer above as its mask.
    BindMaskToAbove,
    /// Insert a paintable mask layer directly below and bind it.
    NewMaskLayer,
    EditMask,
    Up,
    Down,
    ResetDemo,
    /// Wrap the selection in a boolean group.
    VectorCompound(VectorOp),
    /// Switch the selected boolean group to another operation.
    SetBoolean(VectorOp),
    /// Commit the selected boolean group as one editable shape layer.
    FlattenCompound,
    /// Turn the selected Divide group into separate editable shapes.
    ReleaseDivide,
    /// Cut the visible shapes behind the selected line and keep every piece.
    CutBelow,
    OutlineStroke,
    /// Replace a shape's vector source with its rasterized coverage.
    BakeToRaster,
}

impl LayerAction {
    /// Geometry or structure edits refuse to run on a locked layer.
    fn edits_content(&self) -> bool {
        matches!(
            self,
            Self::VectorCompound(_)
                | Self::SetBoolean(_)
                | Self::FlattenCompound
                | Self::ReleaseDivide
                | Self::CutBelow
                | Self::OutlineStroke
                | Self::BakeToRaster
                | Self::ToggleMask
                | Self::BindMaskToAbove
                | Self::NewMaskLayer
                | Self::EditMask
                | Self::GroupSelected
                | Self::UngroupSelected
        )
    }
}

/// True when the layer at `path` is locked, or any of its ancestors is: locking
/// a group protects everything inside it as well.
pub(crate) fn path_locked(doc: &ns_core::Document, path: &[usize]) -> bool {
    (1..=path.len()).any(|len| doc.path_is_locked(&path[..len]))
}

/// True when any layer in the current selection is locked.
pub(crate) fn selection_locked(doc: &ns_core::Document, state: &EditorState) -> bool {
    state
        .selected
        .as_deref()
        .is_some_and(|path| path_locked(doc, path))
        || state
            .multi_selected
            .iter()
            .any(|path| path_locked(doc, path))
}

/// Toggle the lock on the whole selection. The primary row decides the new
/// state, so a mixed selection locks as one and unlocks as one.
pub(crate) fn toggle_lock_selected(state: &mut EditorState, doc: &mut DocumentRes) {
    let lock = state
        .selected
        .as_deref()
        .and_then(|path| doc.0.layer_by_path(path))
        .map(|layer| !layer.locked)
        .unwrap_or(true);
    let mut paths: Vec<Vec<usize>> = Vec::new();
    if let Some(path) = state.selected.clone() {
        paths.push(path);
    }
    paths.extend(state.multi_selected.iter().cloned());
    paths.sort();
    paths.dedup();
    let mut changed = 0;
    for path in paths {
        if let Some(layer) = doc.0.layer_by_path_mut(&path) {
            if layer.locked != lock {
                layer.locked = lock;
                changed += 1;
            }
        }
    }
    if changed == 0 {
        return;
    }
    state.mark_dirty();
    let suffix = if changed == 1 { "" } else { "s" };
    state.status = if lock {
        format!("Locked {changed} layer{suffix}")
    } else {
        format!("Unlocked {changed} layer{suffix}")
    };
}

/// Delete every selected layer, the Delete key and the row/menu actions share
/// this. Paths are removed deepest-first so sibling indices stay valid, and a
/// selected child is skipped when its parent is selected too (the parent takes
/// it along).
pub(crate) fn delete_selected_layers(state: &mut EditorState, doc: &mut DocumentRes) -> bool {
    let mut paths: Vec<Vec<usize>> = Vec::new();
    if let Some(path) = state.selected.clone() {
        paths.push(path);
    }
    paths.extend(state.multi_selected.iter().cloned());
    paths.retain(|path| !path.is_empty());
    if paths.is_empty() {
        return false;
    }
    // Shallow first: lexicographic order puts a parent before its children, so
    // the containment check below can drop the children.
    paths.sort();
    paths.dedup();
    let mut selected: Vec<Vec<usize>> = Vec::new();
    for path in paths {
        if selected
            .iter()
            .any(|parent| path.len() > parent.len() && path.starts_with(parent))
        {
            continue;
        }
        selected.push(path);
    }
    // The lock exists to make Delete safe, so locked layers stay put.
    let locked = selected
        .iter()
        .filter(|path| path_locked(&doc.0, path))
        .count();
    selected.retain(|path| !path_locked(&doc.0, path));
    if selected.is_empty() {
        if locked > 0 {
            state.status = "Layer is locked · unlock it to delete".into();
        }
        return false;
    }
    // The shallowest path decides where the selection lands afterwards.
    let anchor = selected.first().cloned();
    selected.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| b.cmp(a)));
    state.ensure_undo_gesture(&doc.0);
    let mut removed = 0;
    for path in &selected {
        if let Some(layer) = doc.0.remove_at(path) {
            removed += 1;
            if state.mask_edit_layer == Some(layer.id) {
                end_mask_edit(state);
            }
        }
    }
    if removed == 0 {
        return false;
    }
    // Collapse the selection back into what is still on the canvas.
    state.multi_selected.clear();
    state.node_selection.clear();
    state.node_hover = None;
    state.selected = anchor.and_then(|path| {
        let parent = path[..path.len() - 1].to_vec();
        let count = if parent.is_empty() {
            doc.0.layers.len()
        } else {
            doc.0
                .layer_by_path(&parent)
                .and_then(Layer::children)
                .map(|children| children.len())
                .unwrap_or(0)
        };
        if count == 0 {
            (!parent.is_empty()).then_some(parent)
        } else {
            let mut next = parent;
            next.push(path[path.len() - 1].min(count - 1));
            Some(next)
        }
    });
    state.status = if removed == 1 {
        "Layer deleted".into()
    } else {
        format!("{removed} layers deleted")
    };
    if locked > 0 {
        state.status.push_str(if locked == 1 {
            " · 1 locked layer kept"
        } else {
            " · locked layers kept"
        });
    }
    state.mark_dirty();
    true
}

/// Copy every selected layer and select the copies.
pub(crate) fn duplicate_selected_layers(
    state: &mut EditorState,
    doc: &mut DocumentRes,
) -> Vec<Vec<usize>> {
    let mut paths: Vec<Vec<usize>> = Vec::new();
    if let Some(path) = state.selected.clone() {
        paths.push(path);
    }
    paths.extend(state.multi_selected.iter().cloned());
    duplicate_layers(state, doc, &paths)
}

/// Selected layer paths without duplicates or descendants of another selected
/// layer, in document order. Used by the layer clipboard.
fn selection_roots(doc: &ns_core::Document, state: &EditorState) -> Vec<Vec<usize>> {
    let mut paths: Vec<Vec<usize>> = Vec::new();
    if let Some(path) = state.selected.clone() {
        paths.push(path);
    }
    paths.extend(state.multi_selected.iter().cloned());

    let mut roots: Vec<Vec<usize>> = Vec::new();
    for path in paths {
        if path.is_empty() || doc.layer_by_path(&path).is_none() {
            continue;
        }
        if roots.iter().any(|kept| kept == &path) {
            continue;
        }
        if roots
            .iter()
            .any(|kept| path.len() > kept.len() && path.starts_with(kept))
        {
            continue;
        }
        roots.retain(|kept| !(kept.len() > path.len() && kept.starts_with(&path)));
        roots.push(path);
    }
    roots.sort();
    roots
}

/// Put deep copies of the selected layers on the internal clipboard. Nothing
/// is inserted; Ctrl/Cmd+V creates fresh ids and inserts them at the current
/// insertion point. References between copied layers are remapped on paste.
pub(crate) fn copy_selected_layers(state: &mut EditorState, doc: &DocumentRes) -> bool {
    let roots = selection_roots(&doc.0, state);
    if roots.is_empty() {
        state.status = "Select layers to copy".into();
        return false;
    }
    state.layer_clipboard = roots
        .iter()
        .filter_map(|path| doc.0.layer_by_path(path).cloned())
        .collect();
    state.status = format!(
        "Copied {} layer{}",
        state.layer_clipboard.len(),
        if state.layer_clipboard.len() == 1 {
            ""
        } else {
            "s"
        }
    );
    true
}

/// Paste the internal clipboard into the selected group or above the selected
/// layer. Each paste gets fresh layer ids and its own copied masks and grids.
pub(crate) fn paste_layers(state: &mut EditorState, doc: &mut DocumentRes) -> bool {
    if state.layer_clipboard.is_empty() {
        state.status = "Clipboard is empty".into();
        return false;
    }
    let (parent, start) = insertion_point(&doc.0, state);
    let parent_group = if parent.is_empty() {
        None
    } else {
        doc.0.layer_by_path(&parent).map(|layer| layer.id)
    };

    // Snapshot before changing the document. Pasting is not a pointer gesture,
    // so it needs its own undo entry.
    state.checkpoint_document(&doc.0);

    let mut copies = Vec::with_capacity(state.layer_clipboard.len());
    let mut ids = HashMap::new();
    for original in &state.layer_clipboard {
        let copy = original.duplicate();
        collect_id_pairs(original, &copy, &mut ids);
        copies.push(copy);
    }
    for copy in &mut copies {
        copy.remap_layer_references(&ids);
    }

    let mut inserted = Vec::with_capacity(copies.len());
    for (offset, copy) in copies.into_iter().enumerate() {
        if !doc.0.insert_at(&parent, start + offset, copy) {
            break;
        }
        let mut path = parent.clone();
        path.push(start + offset);
        inserted.push(path);
    }
    if inserted.is_empty() {
        state.status = "Paste failed".into();
        return false;
    }

    state.selected = inserted.first().cloned();
    state.multi_selected = inserted.iter().skip(1).cloned().collect();
    state.node_selection.clear();
    state.node_hover = None;
    if let Some(group) = parent_group {
        state.collapsed.remove(&group);
    }
    state.mark_dirty();
    state.status = format!(
        "Pasted {} layer{}",
        inserted.len(),
        if inserted.len() == 1 { "" } else { "s" }
    );
    true
}

/// Duplicate `paths` in place: each copy lands directly above its original and
/// the copies become the selection.
///
/// A selected child is skipped when its parent is selected too, because the
/// parent's copy already contains it. References between the copies (a
/// clipping mask and its source, boolean operands) are remapped so the
/// duplicated set keeps working as a unit; references to layers outside the
/// set keep pointing at the originals.
pub(crate) fn duplicate_layers(
    state: &mut EditorState,
    doc: &mut DocumentRes,
    paths: &[Vec<usize>],
) -> Vec<Vec<usize>> {
    state.ensure_undo_gesture(&doc.0);
    let mut ordered: Vec<Vec<usize>> = Vec::new();
    for path in paths {
        if path.is_empty() || doc.0.layer_by_path(path).is_none() {
            continue;
        }
        if ordered.iter().any(|kept| kept == path) {
            continue;
        }
        if ordered
            .iter()
            .any(|kept| path.len() > kept.len() && path.starts_with(kept))
        {
            continue;
        }
        ordered.retain(|kept| !(kept.len() > path.len() && kept.starts_with(path)));
        ordered.push(path.clone());
    }
    if ordered.is_empty() {
        return Vec::new();
    }
    // Document order keeps sibling indices predictable while inserting.
    ordered.sort();

    // Clone first: the copies need the complete old -> new id map before
    // cross-references between them can be remapped.
    let mut copies: Vec<(Vec<usize>, Layer)> = Vec::with_capacity(ordered.len());
    let mut ids: HashMap<LayerId, LayerId> = HashMap::new();
    for path in &ordered {
        let Some(original) = doc.0.layer_by_path(path) else {
            continue;
        };
        let copy = original.duplicate();
        collect_id_pairs(original, &copy, &mut ids);
        if original.is_group() && state.collapsed.contains(&original.id) {
            state.collapsed.insert(copy.id);
        }
        copies.push((path.clone(), copy));
    }
    for (_, copy) in &mut copies {
        copy.remap_layer_references(&ids);
    }

    let mut inserted = Vec::with_capacity(copies.len());
    let mut shifted: HashMap<Vec<usize>, usize> = HashMap::new();
    for (path, copy) in copies {
        let parent = path[..path.len() - 1].to_vec();
        let index = path[path.len() - 1];
        // Copies already inserted in this parent push later originals down.
        let offset = shifted.get(&parent).copied().unwrap_or(0);
        let target = index + 1 + offset;
        if !doc.0.insert_at(&parent, target, copy) {
            continue;
        }
        shifted.insert(parent.clone(), offset + 1);
        let mut new_path = parent;
        new_path.push(target);
        inserted.push(new_path);
    }
    if inserted.is_empty() {
        return Vec::new();
    }
    state.selected = inserted.first().cloned();
    state.multi_selected = inserted.iter().skip(1).cloned().collect();
    state.node_selection.clear();
    state.node_hover = None;
    state.mark_dirty();
    state.status = if inserted.len() == 1 {
        "Layer duplicated".into()
    } else {
        format!("{} layers duplicated", inserted.len())
    };
    inserted
}

/// Deep copy of one subtree, with references between its own layers remapped
/// to the copies.
pub(crate) fn duplicate_subtree(layer: &Layer) -> Layer {
    let mut copy = layer.duplicate();
    let mut ids = HashMap::new();
    collect_id_pairs(layer, &copy, &mut ids);
    copy.remap_layer_references(&ids);
    copy
}

/// Pair every layer of `original` with the matching copy in `copy`.
fn collect_id_pairs(original: &Layer, copy: &Layer, map: &mut HashMap<LayerId, LayerId>) {
    map.insert(original.id, copy.id);
    if let (Some(originals), Some(copies)) = (original.children(), copy.children()) {
        for (original, copy) in originals.iter().zip(copies) {
            collect_id_pairs(original, copy, map);
        }
    }
}

/// Point `layer` at the sibling directly below it as its mask, hiding that
/// source so it does not render on its own (clipping mask).
fn hide_as_mask(doc: &mut DocumentRes, id: LayerId) {
    if let Some(source) = doc.0.find_layer_mut(id) {
        if source.visible {
            source.visible = false;
            source.hidden_as_mask = true;
        }
    }
}

/// Remove a layer's mask binding and reveal the source if binding hid it.
fn unbind_mask(state: &mut EditorState, doc: &mut DocumentRes, id: LayerId) {
    let Some(source_id) = doc.0.find_layer(id).and_then(|layer| layer.mask_layer) else {
        state.status = "This layer has no mask bound".into();
        return;
    };
    if let Some(layer) = doc.0.find_layer_mut(id) {
        layer.mask_layer = None;
        layer.touch();
    }
    if let Some(source) = doc.0.find_layer_mut(source_id) {
        if source.hidden_as_mask {
            source.visible = true;
            source.hidden_as_mask = false;
        }
    }
    state.status = "Mask unbound".into();
    state.mark_dirty();
}

/// Payload carried while a layer row is being dragged.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LayerDragPayload {
    pub id: LayerId,
    /// Alt was held when the drag started: drop a copy instead of moving.
    pub copy: bool,
}

/// Where a dragged layer should be inserted.
#[derive(Clone, Copy, Debug)]
pub(crate) enum LayerDropTarget {
    /// Insert at `index` in the children of `parent`; `None` means the root.
    Before {
        parent: Option<LayerId>,
        index: usize,
    },
    /// Insert at the end of a group's children.
    IntoGroup(LayerId),
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct LayerDrop {
    pub payload: LayerDragPayload,
    pub target: LayerDropTarget,
}

pub(crate) fn path_parent_id(doc: &ns_core::Document, path: &[usize]) -> Option<LayerId> {
    if path.len() > 1 {
        doc.layer_by_path(&path[..path.len() - 1])
            .map(|layer| layer.id)
    } else {
        None
    }
}

/// True when `source` may be inserted into `target_parent` without creating a
/// cycle or dropping into a non-group.
pub(crate) fn can_drop_layer(
    doc: &ns_core::Document,
    source: LayerId,
    target_parent: Option<LayerId>,
) -> bool {
    let Some(source_path) = doc.find_path(source) else {
        return false;
    };
    let Some(target_id) = target_parent else {
        return true;
    };
    let Some(target_path) = doc.find_path(target_id) else {
        return false;
    };
    if !doc.find_layer(target_id).is_some_and(Layer::is_group) {
        return false;
    }
    // A group cannot be moved into itself or one of its descendants.
    !target_path.starts_with(&source_path)
}

/// Move a dragged layer to a validated position. Returns false for no-ops and
/// invalid targets; callers can use that to avoid marking the document dirty.
pub(crate) fn apply_layer_drop(
    state: &mut EditorState,
    doc: &mut DocumentRes,
    drop: LayerDrop,
) -> bool {
    state.ensure_undo_gesture(&doc.0);
    let Some(source_path) = doc.0.find_path(drop.payload.id) else {
        return false;
    };
    let Some(&source_index) = source_path.last() else {
        return false;
    };
    let source_parent_id = path_parent_id(&doc.0, &source_path);

    let (target_parent_id, mut target_index) = match drop.target {
        LayerDropTarget::Before { parent, index } => (parent, index),
        LayerDropTarget::IntoGroup(group_id) => {
            let Some(group) = doc.0.find_layer(group_id) else {
                return false;
            };
            let Some(children) = group.children() else {
                return false;
            };
            (Some(group_id), children.len())
        }
    };
    if !can_drop_layer(&doc.0, drop.payload.id, target_parent_id) {
        return false;
    }

    // Alt-drag duplicates: the layer stays where it is and the copy lands at
    // the drop position, so no index adjustment for a removal is needed.
    if drop.payload.copy {
        let Some(layer) = doc.0.find_layer(drop.payload.id) else {
            return false;
        };
        let copy = duplicate_subtree(layer);
        let copy_name = copy.name.clone();
        let parent_path = match target_parent_id {
            Some(id) => doc.0.find_path(id).unwrap_or_default(),
            None => Vec::new(),
        };
        let count = if parent_path.is_empty() {
            doc.0.layers.len()
        } else {
            doc.0
                .layer_by_path(&parent_path)
                .and_then(Layer::children)
                .map(|children| children.len())
                .unwrap_or(0)
        };
        let index = target_index.min(count);
        if !doc.0.insert_at(&parent_path, index, copy) {
            return false;
        }
        let mut new_path = parent_path;
        new_path.push(index);
        state.selected = Some(new_path);
        state.multi_selected.clear();
        state.node_selection.clear();
        state.node_hover = None;
        if let Some(group_id) = target_parent_id {
            state.collapsed.remove(&group_id);
        }
        state.status = format!("Duplicated {copy_name}");
        state.mark_dirty();
        return true;
    }

    let same_parent = source_parent_id == target_parent_id;
    if same_parent && source_index < target_index {
        target_index -= 1;
    }
    if same_parent && source_index == target_index {
        return false;
    }
    let source_name = doc
        .0
        .find_layer(drop.payload.id)
        .map(|layer| layer.name.clone())
        .unwrap_or_else(|| "Layer".into());
    let target_name = target_parent_id.and_then(|id| {
        doc.0.find_layer(id).map(|layer| {
            if layer.name.is_empty() {
                "Group".into()
            } else {
                layer.name.clone()
            }
        })
    });

    let Some(layer) = doc.0.remove_at(&source_path) else {
        return false;
    };
    let parent_path = match target_parent_id {
        Some(id) => match doc.0.find_path(id) {
            Some(path) => path,
            None => {
                // This should be unreachable after the cycle check, but avoid
                // losing the layer if the target disappears.
                doc.0
                    .insert_at(&source_path[..source_path.len() - 1], source_index, layer);
                return false;
            }
        },
        None => Vec::new(),
    };
    let count = if parent_path.is_empty() {
        doc.0.layers.len()
    } else {
        doc.0
            .layer_by_path(&parent_path)
            .and_then(Layer::children)
            .map(|children| children.len())
            .unwrap_or(0)
    };
    target_index = target_index.min(count);
    doc.0.insert_at(&parent_path, target_index, layer);

    let mut new_path = parent_path;
    new_path.push(target_index);
    state.selected = Some(new_path);
    state.multi_selected.clear();
    state.node_selection.clear();
    state.node_hover = None;
    if let Some(group_id) = target_parent_id {
        state.collapsed.remove(&group_id);
    }
    state.mark_dirty();
    state.status = match (same_parent, target_name) {
        (true, _) => format!("Reordered {source_name}"),
        (false, Some(group)) => format!("Moved {source_name} into {group}"),
        (false, None) => format!("Moved {source_name} to root"),
    };
    true
}

pub(crate) fn insertion_point(doc: &ns_core::Document, state: &EditorState) -> (Vec<usize>, usize) {
    if let Some(path) = state.selected.as_deref() {
        if let Some(layer) = doc.layer_by_path(path) {
            if layer.is_group() {
                let index = layer.children().map(|children| children.len()).unwrap_or(0);
                return (path.to_vec(), index);
            }
            let parent = path[..path.len() - 1].to_vec();
            return (parent, path[path.len() - 1] + 1);
        }
    }
    (Vec::new(), doc.layers.len())
}

pub(crate) fn add_layer(
    doc: &mut DocumentRes,
    state: &mut EditorState,
    mut layer: Layer,
) -> Vec<usize> {
    // New content belongs to the tile being authored. Groups stay shared so
    // they can organise layers from several tiles; Properties can retag any
    // layer, including back to "All tiles".
    if state.udim_enabled && layer.tile.is_none() && !layer.is_group() {
        layer.tile = Some(doc.0.active_tile.max(ns_core::BASE_UDIM_TILE));
    }
    state.ensure_undo_gesture(&doc.0);
    let (parent, index) = insertion_point(&doc.0, state);
    doc.0.insert_at(&parent, index, layer);
    state.multi_selected.clear();
    state.node_selection.clear();
    state.node_hover = None;
    if !parent.is_empty() {
        if let Some(group) = doc.0.layer_by_path(&parent) {
            state.collapsed.remove(&group.id);
        }
    }
    let mut path = parent;
    path.push(index);
    state.selected = Some(path.clone());
    state.mark_dirty();
    path
}

/// Create a boolean group from the selected sibling shapes. The originals are
/// moved into a group, stay editable, and the primary selection becomes the
/// bottom shape whose bevel style drives the combined result.
fn apply_vector_compound(
    op: VectorOp,
    state: &mut EditorState,
    doc: &mut DocumentRes,
    multi: &[Vec<usize>],
) -> bool {
    let Some(primary) = state.selected.clone() else {
        state.status = "Select two or more vector layers first".into();
        return false;
    };
    let mut paths = multi.to_vec();
    paths.push(primary.clone());
    paths.retain(|path| doc.0.layer_by_path(path).is_some());
    paths.sort_by_key(|path| path.last().copied().unwrap_or(0));
    paths.dedup();
    if paths.len() < 2 {
        state.status = "Select two or more vector layers first".into();
        return false;
    }
    if op == VectorOp::Divide && paths.len() > 8 {
        state.status = "Divide supports up to 8 shapes".into();
        return false;
    }
    let depth = paths[0].len();
    if depth == 0 || paths.iter().any(|path| path.len() != depth) {
        state.status = "Booleans need sibling layers".into();
        return false;
    }
    let parent = paths[0][..depth - 1].to_vec();
    if paths.iter().any(|path| path[..depth - 1] != parent[..]) {
        state.status = "Booleans need sibling layers".into();
        return false;
    }
    let mut primary_id = None;
    for path in &paths {
        let Some(layer) = doc.0.layer_by_path(path) else {
            return false;
        };
        if path == &primary {
            primary_id = Some(layer.id);
        }
        if layer.is_path_guide() {
            state.status = "Path guides steer operations; they cannot be boolean operands".into();
            return false;
        }
        if !layer.has_shape_geometry() {
            state.status = "Booleans need shapes or shape groups".into();
            return false;
        }
    }
    let Some(primary_id) = primary_id else {
        return false;
    };
    let Some(group_path) = doc.0.group_many_at(&paths) else {
        state.status = "Could not create the boolean group".into();
        return false;
    };
    let Some(group) = doc.0.layer_by_path_mut(&group_path) else {
        return false;
    };
    let group_id = group.id;
    if let LayerKind::Group { children } = &mut group.kind {
        // The primary selection is the base for Subtract and supplies the
        // bevel/style parameters for every operation. Cut booleans always use
        // the bottom-most shape as the base, so they keep the stack order.
        if !op.is_cut() {
            if let Some(position) = children.iter().position(|child| child.id == primary_id) {
                if position != 0 {
                    let base = children.remove(position);
                    children.insert(0, base);
                }
            }
        }
    }
    group.vector_op = Some(op);
    group.name = format!("{} boolean", op.short_label());
    group.touch();
    if !group_path.is_empty() && group_path.len() > 1 {
        if let Some(parent) = doc.0.layer_by_path(&group_path[..group_path.len() - 1]) {
            state.collapsed.remove(&parent.id);
        }
    }
    state.selected = Some(group_path);
    state.multi_selected.clear();
    state.collapsed.remove(&group_id);
    state.mark_dirty();
    state.status = format!(
        "Boolean {} · {} shapes stay editable",
        op.label(),
        paths.len()
    );
    true
}

/// Create a new shape layer containing the stroke outline of the selected
/// vector shape. The source layer is left untouched.
pub(crate) fn outline_selected_stroke(
    state: &mut EditorState,
    doc: &mut DocumentRes,
    width: f32,
) -> bool {
    let Some(path) = state.selected.clone() else {
        return false;
    };
    let Some(layer) = doc.0.layer_by_path(&path) else {
        return false;
    };
    let LayerKind::Shape(shape) = &layer.kind else {
        state.status = "Select a vector shape to outline".into();
        return false;
    };
    let Some(source) = shape.source.to_path() else {
        state.status = "Raster shapes cannot be outlined".into();
        return false;
    };
    let width = width.clamp(0.5, 256.0);
    let Some(outline) = source.stroke_outline(width) else {
        state.status = "Stroke outline produced no geometry".into();
        return false;
    };

    let mut new_layer = Layer::shape(
        format!(
            "{} stroke",
            if layer.name.is_empty() {
                "Shape"
            } else {
                &layer.name
            }
        ),
        ShapeSource::Path(outline),
    );
    if let LayerKind::Shape(new_shape) = &mut new_layer.kind {
        new_shape.radius_px = (width * 0.45)
            .clamp(1.0, 10.0)
            .min(shape.radius_px.max(1.0));
        new_shape.height_px = shape.height_px;
        new_shape.profile = shape.profile.clone();
        new_shape.invert = shape.invert;
        new_shape.softness_px = shape.softness_px;
    }
    new_layer.visible = layer.visible;
    new_layer.opacity = layer.opacity;
    new_layer.blend = layer.blend;
    new_layer.mask_layer = layer.mask_layer;
    add_layer(doc, state, new_layer);
    state.status = format!("Outlined stroke at {width:.0} px");
    true
}

#[derive(Default)]
struct BakeStats {
    baked: usize,
    merged: usize,
    already: usize,
    locked: usize,
    failed: Option<&'static str>,
}

/// True when `layer` or anything inside it is locked.
///
/// A group bakes into one layer, so a single locked child cannot be skipped
/// without changing the result; the whole group waits until it is unlocked.
fn tree_has_locked_layer(layer: &ns_core::Layer) -> bool {
    layer.locked
        || layer
            .children()
            .is_some_and(|children| children.iter().any(tree_has_locked_layer))
}

/// Bake every vector shape at `path` to pixels.
///
/// A group merges into one baked normal layer, keeping its stack entry, blend,
/// opacity and masks, so the normal result does not change. A single shape
/// keeps its layer and only swaps its editable source for raster pixels.
fn bake_layer_tree(doc: &mut ns_core::Document, path: &[usize], stats: &mut BakeStats) {
    let Some(layer) = doc.layer_by_path(path) else {
        return;
    };
    if tree_has_locked_layer(layer) {
        stats.locked += 1;
        return;
    }
    let is_group = layer.is_group();
    if is_group {
        match doc.bake_group_to_pixels(path) {
            Some(_) => stats.merged += 1,
            None => stats.failed = Some("the group has no surface to bake"),
        }
        return;
    }
    let size = doc.size;
    let Some(layer) = doc.layer_by_path_mut(path) else {
        return;
    };
    let LayerKind::Shape(shape) = &mut layer.kind else {
        return;
    };
    match shape.bake_to_raster(size) {
        Ok(()) => {
            layer.touch();
            stats.baked += 1;
        }
        Err("already a raster layer") => stats.already += 1,
        Err(reason) => stats.failed = Some(reason),
    }
}

/// Bake the whole selection: the primary row plus every Ctrl/Cmd row, with
/// groups contributing all of their vector descendants.
pub(crate) fn bake_selected_to_pixels(state: &mut EditorState, doc: &mut DocumentRes) {
    let primary = state.selected.clone();
    let mut roots = std::mem::take(&mut state.multi_selected);
    if let Some(path) = primary.clone() {
        roots.push(path);
    }
    roots.sort();
    roots.dedup();
    if roots.is_empty() {
        state.status = "Bake to pixels: select a shape or a group of shapes".into();
        return;
    }
    let mut stats = BakeStats::default();
    for path in &roots {
        bake_layer_tree(&mut doc.0, path, &mut stats);
    }
    // Keep the selection: the user usually bakes several rows and then keeps
    // working with the same set.
    state.multi_selected = if primary.is_some() {
        roots
            .iter()
            .filter(|path| Some(*path) != primary.as_ref())
            .cloned()
            .collect()
    } else {
        roots
    };
    state
        .multi_selected
        .retain(|path| doc.0.layer_by_path(path).is_some());

    if stats.baked > 0 || stats.merged > 0 {
        let mut parts = Vec::new();
        if stats.baked > 0 {
            let plural = if stats.baked == 1 { "" } else { "s" };
            parts.push(format!("Baked {} shape{plural} to pixels", stats.baked));
        }
        if stats.merged > 0 {
            let plural = if stats.merged == 1 { "" } else { "s" };
            parts.push(format!("Merged {} group{plural} to pixels", stats.merged));
        }
        let mut message = parts.join(" · ");
        if stats.locked > 0 {
            message.push_str(&format!(" · {} locked skipped", stats.locked));
        }
        state.status = message;
        state.mark_dirty();
    } else if let Some(reason) = stats.failed {
        state.status = format!("Bake to pixels: {reason}");
    } else if stats.already > 0 {
        state.status = "Bake to pixels: the selection is already raster".into();
    } else if stats.locked > 0 {
        state.status = "Bake to pixels: locked layers are skipped".into();
    } else {
        state.status = "Bake to pixels: the selection has no vector shapes".into();
    }
}

pub(crate) fn apply_layer_action(
    action: LayerAction,
    state: &mut EditorState,
    doc: &mut DocumentRes,
) {
    if action.edits_content() && selection_locked(&doc.0, state) {
        state.status = "Layer is locked · unlock it to edit".into();
        return;
    }
    state.ensure_undo_gesture(&doc.0);
    let size = doc.0.size.x as f32;
    let scale = size / 1024.0;
    let center = Vec2::splat(size * 0.5);
    let multi = std::mem::take(&mut state.multi_selected);
    state.node_selection.clear();
    state.node_hover = None;

    match action {
        LayerAction::AddRect => {
            let layer = Layer::shape(
                "panel",
                ShapeSource::Primitive(Primitive::Rect {
                    center,
                    size: Vec2::new(420.0, 300.0) * scale,
                    corner_radius: 32.0 * scale,
                }),
            );
            push_shape(doc, state, layer, scale);
        }
        LayerAction::AddEllipse => {
            let layer = Layer::shape(
                "dial",
                ShapeSource::Primitive(Primitive::Ellipse {
                    center,
                    radii: Vec2::splat(150.0 * scale),
                }),
            );
            push_shape(doc, state, layer, scale);
        }
        LayerAction::AddStar => {
            let layer = Layer::shape(
                "star",
                ShapeSource::Primitive(Primitive::Star {
                    center,
                    inner_radius: 44.0 * scale,
                    outer_radius: 110.0 * scale,
                    points: 5,
                    rotation: -std::f32::consts::FRAC_PI_2,
                }),
            );
            push_shape(doc, state, layer, scale);
        }
        LayerAction::NewGroup => {
            let (parent, _) = insertion_point(&doc.0, state);
            let group = Layer::group("Group", Vec::new());
            let id = group.id;
            let path = add_layer(doc, state, group);
            state.collapsed.remove(&id);
            if !parent.is_empty() {
                if let Some(parent_group) = doc.0.layer_by_path(&parent) {
                    state.collapsed.remove(&parent_group.id);
                }
            }
            let _ = path;
        }
        LayerAction::GroupSelected => {
            let Some(primary) = state.selected.clone() else {
                return;
            };
            let mut paths = multi;
            paths.push(primary);
            if let Some(group_path) = doc.0.group_many_at(&paths) {
                state.selected = Some(group_path);
                state.mark_dirty();
            }
        }
        LayerAction::UngroupSelected => {
            if let Some(path) = state.selected.clone() {
                let is_group = doc.0.layer_by_path(&path).is_some_and(Layer::is_group);
                if !is_group {
                    return;
                }
                let first = doc.0.ungroup_at(&path);
                state.selected = match first {
                    Some(child) => Some(child),
                    None if path.len() > 1 => Some(path[..path.len() - 1].to_vec()),
                    None => None,
                };
                state.mark_dirty();
            }
        }
        LayerAction::Delete => {
            delete_selected_layers(state, doc);
        }
        LayerAction::ToggleLock => {
            toggle_lock_selected(state, doc);
        }
        LayerAction::Duplicate => {
            let Some(primary) = state.selected.clone() else {
                return;
            };
            let mut paths = multi;
            paths.push(primary);
            duplicate_layers(state, doc, &paths);
        }
        LayerAction::ToggleMask => {
            let Some(path) = state.selected.clone() else {
                return;
            };
            let Some(id) = doc.0.layer_by_path(&path).map(|layer| layer.id) else {
                return;
            };
            if doc
                .0
                .find_layer(id)
                .is_some_and(|layer| layer.mask_layer.is_some())
            {
                unbind_mask(state, doc, id);
                return;
            }
            let (last, parents) = match path.split_last() {
                Some(parts) => (*parts.0, parts.1.to_vec()),
                None => return,
            };
            if last == 0 {
                state.status = "No layer below to use as a mask".into();
                return;
            }
            let siblings = if parents.is_empty() {
                &doc.0.layers
            } else {
                match doc.0.layer_by_path(&parents).and_then(Layer::children) {
                    Some(children) => children,
                    None => return,
                }
            };
            let Some(below_id) = siblings.get(last - 1).map(|layer| layer.id) else {
                state.status = "No layer below to use as a mask".into();
                return;
            };
            if let Some(layer) = doc.0.find_layer_mut(id) {
                layer.mask_layer = Some(below_id);
                layer.touch();
            }
            hide_as_mask(doc, below_id);
            state.mark_dirty();
            state.status = "Layer below bound as mask".into();
        }
        LayerAction::BindMaskToAbove => {
            let Some(path) = state.selected.clone() else {
                return;
            };
            let Some(source_id) = doc.0.layer_by_path(&path).map(|layer| layer.id) else {
                return;
            };
            let Some((last, parents)) = path.split_last().map(|(l, p)| (*l, p.to_vec())) else {
                return;
            };
            let siblings = if parents.is_empty() {
                &doc.0.layers
            } else {
                match doc.0.layer_by_path(&parents).and_then(Layer::children) {
                    Some(children) => children,
                    None => return,
                }
            };
            let Some(above_id) = siblings.get(last + 1).map(|layer| layer.id) else {
                state.status = "No layer above to bind this mask to".into();
                return;
            };
            if let Some(layer) = doc.0.find_layer_mut(above_id) {
                layer.mask_layer = Some(source_id);
                layer.touch();
                state.status = format!("{} now masks the layer above", layer.name);
            }
            hide_as_mask(doc, source_id);
            state.selected = Some(path);
            state.mark_dirty();
        }
        LayerAction::NewMaskLayer => {
            let Some(path) = state.selected.clone() else {
                return;
            };
            let Some((last, parents)) = path.split_last().map(|(l, p)| (*l, p.to_vec())) else {
                return;
            };
            let Some(target_id) = doc.0.layer_by_path(&path).map(|layer| layer.id) else {
                return;
            };
            let size = doc.0.size;
            let mut mask_layer = Layer::flat("Mask");
            mask_layer.mask = Some(Grid::new(size.x, size.y, 1.0));
            mask_layer.touch_mask();
            let mask_id = mask_layer.id;
            if !doc.0.insert_at(&parents, last, mask_layer) {
                return;
            }
            if let Some(target) = doc.0.find_layer_mut(target_id) {
                target.mask_layer = Some(mask_id);
                target.touch();
            }
            hide_as_mask(doc, mask_id);
            // The new mask sits below the target; keep the target selected so
            // its clip settings (offset, feather) are right at hand.
            state.selected = Some({
                let mut p = parents.clone();
                p.push(last + 1);
                p
            });
            state.status = "Mask layer added below · paint it to cut".into();
            state.mark_dirty();
        }
        LayerAction::EditMask => {
            let Some(path) = state.selected.clone() else {
                return;
            };
            let Some(id) = doc.0.layer_by_path(&path).map(|layer| layer.id) else {
                return;
            };
            begin_mask_edit(state, doc, id);
        }
        LayerAction::Up | LayerAction::Down => {
            let delta = if matches!(action, LayerAction::Up) {
                1
            } else {
                -1
            };
            if let Some(path) = state.selected.clone() {
                if doc.0.move_at(&path, delta) {
                    let mut next = path;
                    let last = next.len() - 1;
                    next[last] = (next[last] as isize + delta) as usize;
                    state.selected = Some(next);
                    state.mark_dirty();
                }
            }
        }
        LayerAction::VectorCompound(op) => {
            apply_vector_compound(op, state, doc, &multi);
        }
        LayerAction::SetBoolean(op) => {
            let Some(path) = state.selected.clone() else {
                return;
            };
            let Some(layer) = doc.0.layer_by_path_mut(&path) else {
                return;
            };
            if !layer.is_group() || layer.vector_op == Some(op) {
                return;
            }
            layer.vector_op = Some(op);
            layer.touch();
            state.status = format!("Boolean · {}", op.label());
            state.mark_dirty();
        }
        LayerAction::FlattenCompound => {
            let Some(path) = state.selected.clone() else {
                return;
            };
            let cut = doc
                .0
                .layer_by_path(&path)
                .is_some_and(|layer| layer.vector_op.is_some_and(VectorOp::is_cut));
            let baked = if cut {
                doc.0.flatten_cut_compound(&path)
            } else {
                doc.0.flatten_vector_compound(&path)
            };
            match baked {
                Some(new_path) => {
                    state.selected = Some(new_path);
                    state.multi_selected.clear();
                    state.status = if cut {
                        "Cut boolean baked into editable shapes".into()
                    } else {
                        "Boolean baked into one editable shape".into()
                    };
                    state.mark_dirty();
                }
                None => {
                    state.status = if cut {
                        "Select a cut boolean group to bake".into()
                    } else {
                        "Select a boolean group to bake".into()
                    };
                }
            }
        }
        LayerAction::ReleaseDivide => {
            let Some(path) = state.selected.clone() else {
                return;
            };
            match doc.0.release_divide_compound(&path) {
                Some(paths) => {
                    state.selected = paths.first().cloned();
                    state.multi_selected = paths.iter().skip(1).cloned().collect();
                    state.status = format!("Divide split into {} editable shapes", paths.len());
                    state.mark_dirty();
                }
                None => {
                    state.status = "Select a Divide group with two or more regions".into();
                }
            }
        }
        LayerAction::CutBelow => {
            let Some(path) = state.selected.clone() else {
                return;
            };
            match doc.0.cut_layers_below(&path) {
                Some(outcome) => {
                    state.selected = outcome.paths.first().cloned();
                    state.multi_selected = outcome.paths.iter().skip(1).cloned().collect();
                    state.status = if outcome.paths.is_empty() {
                        format!(
                            "Cut {} shape{} away",
                            outcome.targets,
                            if outcome.targets == 1 { "" } else { "s" }
                        )
                    } else {
                        format!(
                            "Cut {} shape{} into {} piece{}",
                            outcome.targets,
                            if outcome.targets == 1 { "" } else { "s" },
                            outcome.paths.len(),
                            if outcome.paths.len() == 1 { "" } else { "s" }
                        )
                    };
                    state.mark_dirty();
                }
                None => {
                    state.status =
                        "Cut needs a stroked line or shape overlapping the layers below".into();
                }
            }
        }
        LayerAction::OutlineStroke => {
            outline_selected_stroke(state, doc, state.stroke_width);
        }
        LayerAction::BakeToRaster => {
            // `multi` was taken from the state above, so hand the full set to
            // the bake helper and let it restore the selection afterwards.
            state.multi_selected = multi;
            bake_selected_to_pixels(state, doc);
        }
        LayerAction::ResetDemo => {
            doc.0 = demo_document(doc.0.size.x).into();
            state.collapsed.clear();
            state.selected = doc.0.layers.len().checked_sub(1).map(|index| vec![index]);
            state.mark_dirty();
        }
    }
}

pub(crate) fn push_shape(
    doc: &mut DocumentRes,
    state: &mut EditorState,
    mut layer: Layer,
    scale: f32,
) {
    if let LayerKind::Shape(shape) = &mut layer.kind {
        shape.radius_px = (28.0 * scale).clamp(1.0, 10.0);
        shape.height_px = 14.0 * scale;
    }
    add_layer(doc, state, layer);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drag(id: LayerId) -> LayerDragPayload {
        LayerDragPayload { id, copy: false }
    }

    fn copy_drag(id: LayerId) -> LayerDragPayload {
        LayerDragPayload { id, copy: true }
    }

    #[test]
    fn new_layers_follow_the_active_udim_tile() {
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(32)));
        doc.0.active_tile = 1002;
        let mut state = EditorState {
            udim_enabled: true,
            ..Default::default()
        };

        add_layer(&mut doc, &mut state, Layer::flat("paint"));
        add_layer(&mut doc, &mut state, Layer::group("group", Vec::new()));

        assert_eq!(doc.0.layers[0].tile, Some(1002));
        assert_eq!(doc.0.layers[1].tile, None, "groups stay shared");
    }

    #[test]
    fn a_cut_boolean_keeps_the_bottom_shape_as_the_base() {
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(256)));
        for (name, center, size) in [
            ("plate", Vec2::splat(128.0), Vec2::new(200.0, 160.0)),
            ("pocket", Vec2::splat(128.0), Vec2::new(80.0, 60.0)),
        ] {
            doc.0.layers.push(Layer::shape(
                name,
                ShapeSource::Primitive(Primitive::Rect {
                    center,
                    size,
                    corner_radius: 12.0,
                }),
            ));
        }
        // The pocket is selected last (primary); the plate must still be the
        // base because cut booleans cut the bottom-most shape.
        let mut state = EditorState {
            selected: Some(vec![1]),
            multi_selected: vec![vec![0]],
            ..Default::default()
        };

        apply_layer_action(
            LayerAction::VectorCompound(VectorOp::PartialCut),
            &mut state,
            &mut doc,
        );

        assert_eq!(doc.0.layers.len(), 1);
        let group = &doc.0.layers[0];
        assert_eq!(group.vector_op, Some(VectorOp::PartialCut));
        let children = group.children().expect("boolean group children");
        assert_eq!(children[0].name, "plate");
        assert_eq!(children[1].name, "pocket");
    }

    #[test]
    fn flattening_a_cut_boolean_is_refused() {
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(128)));
        let mut group = Layer::group(
            "cut",
            vec![
                Layer::shape(
                    "plate",
                    ShapeSource::Primitive(Primitive::Rect {
                        center: Vec2::splat(64.0),
                        size: Vec2::splat(80.0),
                        corner_radius: 8.0,
                    }),
                ),
                Layer::shape(
                    "pocket",
                    ShapeSource::Primitive(Primitive::Rect {
                        center: Vec2::splat(64.0),
                        size: Vec2::splat(40.0),
                        corner_radius: 6.0,
                    }),
                ),
            ],
        );
        group.vector_op = Some(VectorOp::PartialCut);
        doc.0.layers.push(group);

        assert!(doc.0.flatten_vector_compound(&[0]).is_none());
    }

    #[test]
    fn baking_a_cut_boolean_keeps_editable_child_layers() {
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(128)));
        let mut group = Layer::group(
            "cut",
            vec![
                Layer::shape(
                    "plate",
                    ShapeSource::Primitive(Primitive::Rect {
                        center: Vec2::splat(64.0),
                        size: Vec2::splat(80.0),
                        corner_radius: 8.0,
                    }),
                ),
                Layer::shape(
                    "pocket",
                    ShapeSource::Primitive(Primitive::Rect {
                        center: Vec2::splat(64.0),
                        size: Vec2::splat(40.0),
                        corner_radius: 6.0,
                    }),
                ),
            ],
        );
        group.vector_op = Some(VectorOp::PartialCut);
        doc.0.layers.push(group);
        let mut state = EditorState {
            selected: Some(vec![0]),
            ..Default::default()
        };

        apply_layer_action(LayerAction::FlattenCompound, &mut state, &mut doc);

        let layer = &doc.0.layers[0];
        assert!(layer.vector_op.is_none());
        let children = layer.children().expect("baked cut keeps its children");
        assert_eq!(children[0].blend, BlendMode::Derivative);
        assert_eq!(children[1].blend, BlendMode::PartialCut);
        assert!(
            state.status.contains("Cut boolean baked"),
            "status was {}",
            state.status
        );
        assert_eq!(state.selected, Some(vec![0]));
    }

    #[test]
    fn bake_to_pixels_rasterizes_a_vector_shape() {
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(64)));
        doc.0.layers.push(Layer::shape(
            "rect",
            ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::splat(32.0),
                size: Vec2::splat(30.0),
                corner_radius: 4.0,
            }),
        ));
        let mut state = EditorState {
            selected: Some(vec![0]),
            ..EditorState::default()
        };

        apply_layer_action(LayerAction::BakeToRaster, &mut state, &mut doc);

        let LayerKind::Shape(shape) = &doc.0.layers[0].kind else {
            panic!("expected a shape")
        };
        assert!(matches!(shape.source, ShapeSource::RasterRegion(_)));
        assert!(
            state.status.contains("Baked 1 shape"),
            "status was {}",
            state.status
        );
    }

    #[test]
    fn bake_to_pixels_bakes_every_layer_in_a_multi_selection() {
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(64)));
        for name in ["a", "b", "c"] {
            doc.0
                .layers
                .push(compound_rect(name, Vec2::splat(32.0), Vec2::splat(20.0)));
        }
        let mut state = EditorState {
            selected: Some(vec![0]),
            multi_selected: vec![vec![2]],
            ..Default::default()
        };

        apply_layer_action(LayerAction::BakeToRaster, &mut state, &mut doc);

        for index in [0, 2] {
            let LayerKind::Shape(shape) = &doc.0.layers[index].kind else {
                panic!("expected a shape")
            };
            assert!(
                matches!(shape.source, ShapeSource::RasterRegion(_)),
                "layer {index} was not baked"
            );
        }
        let LayerKind::Shape(untouched) = &doc.0.layers[1].kind else {
            panic!("expected a shape")
        };
        assert!(matches!(untouched.source, ShapeSource::Primitive(_)));
        assert!(state.status.contains("Baked 2 shapes"), "{}", state.status);
        // The set survives the action so the user can keep working with it.
        assert_eq!(state.selected, Some(vec![0]));
        assert_eq!(state.multi_selected, vec![vec![2]]);
    }

    #[test]
    fn bake_to_pixels_merges_a_group_into_one_layer() {
        let children = vec![
            compound_rect("a", Vec2::splat(24.0), Vec2::splat(20.0)),
            compound_rect("b", Vec2::splat(40.0), Vec2::splat(20.0)),
        ];
        let mut group = Layer::group("group", children);
        group.opacity = 0.5;
        let group_id = group.id;
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(64)));
        doc.0.layers.push(group);
        let before = doc.0.evaluate();
        let mut state = EditorState {
            selected: Some(vec![0]),
            ..Default::default()
        };

        apply_layer_action(LayerAction::BakeToRaster, &mut state, &mut doc);

        assert_eq!(doc.0.layers.len(), 1);
        let layer = &doc.0.layers[0];
        let LayerKind::Shape(shape) = &layer.kind else {
            panic!("a group bake must stay a movable shape layer");
        };
        assert!(matches!(shape.source, ShapeSource::RasterRegion(_)));
        assert!(shape.baked_normal.is_some());
        assert_eq!(layer.id, group_id, "references to the group must survive");
        assert_eq!(layer.name, "group");
        assert!((layer.opacity - 0.5).abs() < 1e-6);
        assert!(
            state.status.contains("Merged 1 group"),
            "status was {}",
            state.status
        );
        assert_eq!(state.selected, Some(vec![0]));

        // Merging is a bake: the composed normal map must not move.
        let after = doc.0.evaluate();
        let mut worst = 0.0f32;
        for (a, b) in before.data.iter().zip(after.data.iter()) {
            worst = worst.max((*a - *b).length());
        }
        assert!(worst < 1e-5, "merged group moved normals by {worst}");
    }

    #[test]
    fn bake_to_pixels_skips_a_group_with_locked_children() {
        let mut locked = compound_rect("locked", Vec2::splat(24.0), Vec2::splat(20.0));
        locked.locked = true;
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(64)));
        doc.0.layers.push(Layer::group(
            "group",
            vec![
                locked,
                compound_rect("open", Vec2::splat(40.0), Vec2::splat(20.0)),
            ],
        ));
        let mut state = EditorState {
            selected: Some(vec![0]),
            ..Default::default()
        };

        apply_layer_action(LayerAction::BakeToRaster, &mut state, &mut doc);

        let LayerKind::Group { children } = &doc.0.layers[0].kind else {
            panic!("a locked child keeps the whole group editable")
        };
        assert!(children[0].locked);
        for child in children {
            let LayerKind::Shape(shape) = &child.kind else {
                panic!("expected a shape")
            };
            assert!(matches!(shape.source, ShapeSource::Primitive(_)));
        }
        assert!(
            state.status.contains("locked"),
            "status was {}",
            state.status
        );
    }

    fn compound_rect(name: &str, center: Vec2, size: Vec2) -> Layer {
        let mut layer = Layer::shape(
            name,
            ShapeSource::Primitive(Primitive::Rect {
                center,
                size,
                corner_radius: 6.0,
            }),
        );
        if let LayerKind::Shape(shape) = &mut layer.kind {
            shape.radius_px = 10.0;
            shape.height_px = 12.0;
        }
        layer
    }

    #[test]
    fn duplicating_puts_copies_above_their_originals() {
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(128)));
        for name in ["a", "b", "c"] {
            doc.0
                .layers
                .push(compound_rect(name, Vec2::splat(64.0), Vec2::splat(40.0)));
        }
        let mut state = EditorState {
            selected: Some(vec![0]),
            multi_selected: vec![vec![2]],
            ..Default::default()
        };

        let copies = duplicate_selected_layers(&mut state, &mut doc);
        assert_eq!(copies, vec![vec![1], vec![4]]);
        let names: Vec<&str> = doc
            .0
            .layers
            .iter()
            .map(|layer| layer.name.as_str())
            .collect();
        assert_eq!(names, ["a", "a copy", "b", "c", "c copy"]);
        assert_eq!(state.selected, Some(vec![1]));
        assert_eq!(state.multi_selected, vec![vec![4]]);
        assert_eq!(state.status, "2 layers duplicated");
    }

    #[test]
    fn duplicating_a_group_copies_the_whole_subtree() {
        let child_a = compound_rect("a", Vec2::splat(40.0), Vec2::splat(20.0));
        let child_b = compound_rect("b", Vec2::splat(60.0), Vec2::splat(20.0));
        let original_ids = [child_a.id, child_b.id];
        let group = Layer::group("group", vec![child_a, child_b]);
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(128)));
        doc.0.layers.push(group);
        let mut state = EditorState {
            selected: Some(vec![0]),
            ..Default::default()
        };

        let copies = duplicate_selected_layers(&mut state, &mut doc);
        assert_eq!(copies, vec![vec![1]]);
        let copy = &doc.0.layers[1];
        assert_eq!(copy.name, "group copy");
        let children = copy.children().expect("still a group");
        assert_eq!(
            children
                .iter()
                .map(|layer| layer.name.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert!(children
            .iter()
            .all(|child| !original_ids.contains(&child.id)));
    }

    #[test]
    fn a_copied_mask_points_at_the_copied_source() {
        let source = compound_rect("source", Vec2::splat(50.0), Vec2::splat(20.0));
        let source_id = source.id;
        let mut masked = compound_rect("masked", Vec2::splat(60.0), Vec2::splat(20.0));
        masked.mask_layer = Some(source_id);
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(128)));
        doc.0.layers.push(masked);
        doc.0.layers.push(source);
        let mut state = EditorState {
            selected: Some(vec![0]),
            multi_selected: vec![vec![1]],
            ..Default::default()
        };

        let copies = duplicate_selected_layers(&mut state, &mut doc);
        // masked copy at 1, source copy at 3.
        assert_eq!(copies, vec![vec![1], vec![3]]);
        let masked_copy = doc.0.layer_by_path(&copies[0]).expect("masked copy");
        let source_copy = doc.0.layer_by_path(&copies[1]).expect("source copy");
        assert_eq!(masked_copy.mask_layer, Some(source_copy.id));
        assert_eq!(
            doc.0.layers[0].mask_layer,
            Some(source_id),
            "the original keeps the original source"
        );
    }

    #[test]
    fn copy_and_paste_creates_independent_reference_remapped_layers() {
        let source = compound_rect("source", Vec2::splat(50.0), Vec2::splat(20.0));
        let source_id = source.id;
        let mut masked = compound_rect("masked", Vec2::splat(60.0), Vec2::splat(20.0));
        masked.mask_layer = Some(source_id);
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(128)));
        doc.0.layers.push(masked);
        doc.0.layers.push(source);
        let mut state = EditorState {
            selected: Some(vec![0]),
            multi_selected: vec![vec![1]],
            ..Default::default()
        };

        assert!(copy_selected_layers(&mut state, &doc));
        assert_eq!(state.layer_clipboard.len(), 2);
        assert!(paste_layers(&mut state, &mut doc));

        assert_eq!(doc.0.layers.len(), 4);
        assert_eq!(state.selected, Some(vec![1]));
        assert_eq!(state.multi_selected, vec![vec![2]]);
        let masked_copy = doc.0.layer_by_path(&[1]).expect("masked copy");
        let source_copy = doc.0.layer_by_path(&[2]).expect("source copy");
        assert_eq!(masked_copy.name, "masked copy");
        assert_eq!(source_copy.name, "source copy");
        assert_ne!(masked_copy.id, doc.0.layers[0].id);
        assert_ne!(source_copy.id, source_id);
        assert_eq!(
            masked_copy.mask_layer,
            Some(source_copy.id),
            "references between pasted layers point at the pasted source"
        );
        assert_eq!(
            doc.0.layers[0].mask_layer,
            Some(source_id),
            "the original keeps its original source"
        );

        // Pasting again makes another independent set at the insertion point.
        assert!(paste_layers(&mut state, &mut doc));
        assert_eq!(doc.0.layers.len(), 6);

        // Each paste is its own undo step: 6 -> 4 -> 2 layers.
        assert!(state.undo(&mut doc));
        assert_eq!(doc.0.layers.len(), 4);
        assert!(state.undo(&mut doc));
        assert_eq!(doc.0.layers.len(), 2);
    }

    #[test]
    fn a_duplicate_carries_its_own_copy_of_the_painted_mask() {
        let mut layer = compound_rect("masked", Vec2::splat(60.0), Vec2::splat(20.0));
        layer.mask = Some(Grid::new(8, 8, 0.5));
        layer.mask_settings.density = 0.7;
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(128)));
        doc.0.layers.push(layer);
        let mut state = EditorState {
            selected: Some(vec![0]),
            ..Default::default()
        };

        let copies = duplicate_selected_layers(&mut state, &mut doc);
        let (mask_value, density) = {
            let copy = doc.0.layer_by_path(&copies[0]).expect("copy");
            let mask = copy.mask.as_ref().expect("the painted mask came along");
            (*mask.get(2, 2), copy.mask_settings.density)
        };
        assert!((mask_value - 0.5).abs() < 1e-6);
        assert!((density - 0.7).abs() < 1e-6, "mask settings are copied too");

        // The copy owns its grid: painting the original leaves the copy alone.
        doc.0.layers[0].mask.as_mut().unwrap().data[0] = 0.25;
        let copy = doc.0.layer_by_path(&copies[0]).expect("copy");
        assert!((*copy.mask.as_ref().unwrap().get(0, 0) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn an_alt_drop_duplicates_instead_of_moving() {
        let a = compound_rect("a", Vec2::splat(40.0), Vec2::splat(20.0));
        let a_id = a.id;
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(128)));
        doc.0.layers.push(a);
        doc.0
            .layers
            .push(compound_rect("b", Vec2::splat(60.0), Vec2::splat(20.0)));
        let mut state = EditorState::default();

        let changed = apply_layer_drop(
            &mut state,
            &mut doc,
            LayerDrop {
                payload: copy_drag(a_id),
                target: LayerDropTarget::Before {
                    parent: None,
                    index: 2,
                },
            },
        );
        assert!(changed);
        assert_eq!(doc.0.layers.len(), 3, "the original is still there");
        assert_eq!(doc.0.layers[0].id, a_id);
        assert_eq!(doc.0.layers[2].name, "a copy");
        assert_eq!(state.selected, Some(vec![2]));
    }

    #[test]
    fn the_delete_action_removes_every_selected_layer() {
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(128)));
        for name in ["a", "b", "c"] {
            doc.0
                .layers
                .push(compound_rect(name, Vec2::splat(64.0), Vec2::splat(40.0)));
        }
        let mut state = EditorState {
            selected: Some(vec![1]),
            multi_selected: vec![vec![2]],
            ..Default::default()
        };
        assert!(delete_selected_layers(&mut state, &mut doc));
        assert_eq!(doc.0.layers.len(), 1, "both selected layers are gone");
        assert_eq!(doc.0.layers[0].name, "a");
        assert_eq!(
            state.selected,
            Some(vec![0]),
            "selection lands on a neighbour"
        );
        assert!(state.multi_selected.is_empty());
        assert_eq!(state.status, "2 layers deleted");
    }

    #[test]
    fn deleting_a_parent_takes_its_selected_child_with_it() {
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(128)));
        doc.0
            .layers
            .push(compound_rect("base", Vec2::splat(64.0), Vec2::splat(40.0)));
        let child = compound_rect("child", Vec2::splat(64.0), Vec2::splat(20.0));
        doc.0.layers.push(Layer::group("group", vec![child]));
        let mut state = EditorState {
            selected: Some(vec![1]),
            multi_selected: vec![vec![1, 0]],
            ..Default::default()
        };
        assert!(delete_selected_layers(&mut state, &mut doc));
        assert_eq!(doc.0.layers.len(), 1);
        assert_eq!(doc.0.layers[0].name, "base");
        assert_eq!(state.selected, Some(vec![0]));
        assert_eq!(state.status, "Layer deleted");
    }

    #[test]
    fn deleting_without_a_selection_changes_nothing() {
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(128)));
        doc.0
            .layers
            .push(compound_rect("a", Vec2::splat(64.0), Vec2::splat(40.0)));
        let mut state = EditorState::default();
        assert!(!delete_selected_layers(&mut state, &mut doc));
        assert_eq!(doc.0.layers.len(), 1);
        assert!(state.selected.is_none());
    }

    #[test]
    fn live_compound_keeps_operands_and_primary_becomes_the_base() {
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(128)));
        let left = compound_rect("left", Vec2::new(48.0, 64.0), Vec2::splat(72.0));
        let right = compound_rect("right", Vec2::new(80.0, 64.0), Vec2::splat(72.0));
        let left_id = left.id;
        let right_id = right.id;
        doc.0.layers.push(left);
        doc.0.layers.push(right);
        let mut state = EditorState {
            selected: Some(vec![1]),
            multi_selected: vec![vec![0]],
            ..Default::default()
        };

        apply_layer_action(
            LayerAction::VectorCompound(VectorOp::Subtract),
            &mut state,
            &mut doc,
        );

        assert_eq!(doc.0.layers.len(), 1);
        let group = &doc.0.layers[0];
        assert_eq!(group.vector_op, Some(VectorOp::Subtract));
        let children = group.children().expect("compound is a group");
        assert_eq!(children.len(), 2, "operands are preserved");
        assert_eq!(children[0].id, right_id, "primary selection is the base");
        assert_eq!(children[1].id, left_id);
        assert_eq!(state.selected, Some(vec![0]));
    }

    #[test]
    fn flatten_compound_action_commits_one_editable_shape() {
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(128)));
        let mut group = Layer::group(
            "Add compound",
            vec![
                compound_rect("base", Vec2::new(48.0, 64.0), Vec2::splat(72.0)),
                compound_rect("top", Vec2::new(80.0, 64.0), Vec2::splat(72.0)),
            ],
        );
        group.vector_op = Some(VectorOp::Add);
        let group_id = group.id;
        doc.0.layers.push(group);
        let mut state = EditorState {
            selected: Some(vec![0]),
            ..Default::default()
        };

        apply_layer_action(LayerAction::FlattenCompound, &mut state, &mut doc);

        assert_eq!(doc.0.layers.len(), 1);
        assert!(doc.0.find_layer(group_id).is_none());
        let layer = &doc.0.layers[0];
        assert!(layer.vector_op.is_none());
        assert!(
            matches!(&layer.kind, LayerKind::Shape(shape) if matches!(shape.source, ShapeSource::Path(_))),
            "flattened compound should be an editable path"
        );
        assert_eq!(state.selected, Some(vec![0]));
    }

    #[test]
    fn release_divide_action_selects_every_island() {
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(128)));
        let mut group = Layer::group(
            "Divide compound",
            vec![
                compound_rect("a", Vec2::new(48.0, 64.0), Vec2::splat(72.0)),
                compound_rect("b", Vec2::new(80.0, 64.0), Vec2::splat(72.0)),
            ],
        );
        group.vector_op = Some(VectorOp::Divide);
        doc.0.layers.push(group);
        let mut state = EditorState {
            selected: Some(vec![0]),
            ..Default::default()
        };

        apply_layer_action(LayerAction::ReleaseDivide, &mut state, &mut doc);

        assert_eq!(doc.0.layers.len(), 3, "two operands produce three islands");
        assert_eq!(state.selected, Some(vec![0]));
        assert_eq!(state.multi_selected, vec![vec![1], vec![2]]);
        assert!(doc
            .0
            .layers
            .iter()
            .all(|layer| matches!(&layer.kind, LayerKind::Shape(shape) if matches!(shape.source, ShapeSource::Path(_)))));
    }

    #[test]
    fn drag_reorders_siblings() {
        let mut doc = DocumentRes::new(demo_document(64));
        let mut state = EditorState::default();
        let base_id = doc.0.layers[0].id;
        let count = doc.0.layers.len();
        assert!(apply_layer_drop(
            &mut state,
            &mut doc,
            LayerDrop {
                payload: drag(base_id),
                target: LayerDropTarget::Before {
                    parent: None,
                    index: count,
                },
            },
        ));
        assert_eq!(doc.0.layers.last().map(|layer| layer.id), Some(base_id));
        assert_eq!(state.selected, Some(vec![count - 1]));
    }

    #[test]
    fn drag_moves_a_child_out_to_the_root() {
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(64)));
        doc.0.layers.push(Layer::group(
            "Group",
            vec![Layer::flat("lower"), Layer::flat("upper")],
        ));
        doc.0.layers.push(Layer::flat("top"));
        let moved_id = doc.0.layers[0].children().unwrap()[1].id;
        let mut state = EditorState::default();
        assert!(apply_layer_drop(
            &mut state,
            &mut doc,
            LayerDrop {
                payload: drag(moved_id),
                target: LayerDropTarget::Before {
                    parent: None,
                    index: 0,
                },
            },
        ));
        assert_eq!(doc.0.find_path(moved_id), Some(vec![0]));
        assert_eq!(doc.0.layers.len(), 3);
        assert_eq!(
            doc.0.layers[1].children().map(|children| children.len()),
            Some(1)
        );
    }

    #[test]
    fn drag_reorders_children_inside_a_group() {
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(64)));
        doc.0.layers.push(Layer::group(
            "Group",
            vec![Layer::flat("a"), Layer::flat("b"), Layer::flat("c")],
        ));
        let group_id = doc.0.layers[0].id;
        let first_id = doc.0.layers[0].children().unwrap()[0].id;
        let mut state = EditorState::default();
        assert!(apply_layer_drop(
            &mut state,
            &mut doc,
            LayerDrop {
                payload: drag(first_id),
                target: LayerDropTarget::Before {
                    parent: Some(group_id),
                    index: 3,
                },
            },
        ));
        assert_eq!(doc.0.find_path(first_id), Some(vec![0, 2]));
        assert_eq!(state.selected, Some(vec![0, 2]));
    }

    #[test]
    fn drag_moves_a_layer_into_a_group() {
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(64)));
        doc.0
            .layers
            .push(Layer::group("Group", vec![Layer::flat("inside")]));
        doc.0.layers.push(Layer::flat("top"));
        let group_id = doc.0.layers[0].id;
        let moved_id = doc.0.layers[1].id;
        let mut state = EditorState::default();
        assert!(apply_layer_drop(
            &mut state,
            &mut doc,
            LayerDrop {
                payload: drag(moved_id),
                target: LayerDropTarget::IntoGroup(group_id),
            },
        ));
        assert_eq!(doc.0.find_path(moved_id), Some(vec![0, 1]));
        assert_eq!(doc.0.layers.len(), 1);
        assert_eq!(state.selected, Some(vec![0, 1]));
    }

    #[test]
    fn drag_rejects_moving_a_group_into_itself() {
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(64)));
        doc.0
            .layers
            .push(Layer::group("Group", vec![Layer::flat("inside")]));
        let group_id = doc.0.layers[0].id;
        let mut state = EditorState::default();
        assert!(!apply_layer_drop(
            &mut state,
            &mut doc,
            LayerDrop {
                payload: drag(group_id),
                target: LayerDropTarget::IntoGroup(group_id),
            },
        ));
        assert_eq!(doc.0.layers.len(), 1);
    }

    fn rect_layer(name: &str, min: Vec2, max: Vec2) -> Layer {
        Layer::shape(
            name,
            ShapeSource::Primitive(Primitive::Rect {
                center: (min + max) * 0.5,
                size: max - min,
                corner_radius: 0.0,
            }),
        )
    }

    #[test]
    fn binding_the_layer_below_hides_it_and_unbinding_reveals_it() {
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(64)));
        doc.0.layers.push(rect_layer(
            "mask",
            Vec2::new(8.0, 8.0),
            Vec2::new(40.0, 40.0),
        ));
        doc.0.layers.push(rect_layer(
            "detail",
            Vec2::new(4.0, 4.0),
            Vec2::new(60.0, 60.0),
        ));
        let mut state = EditorState {
            selected: Some(vec![1]),
            ..Default::default()
        };

        apply_layer_action(LayerAction::ToggleMask, &mut state, &mut doc);
        let mask_id = doc.0.layers[0].id;
        assert_eq!(doc.0.layers[1].mask_layer, Some(mask_id));
        assert!(!doc.0.layers[0].visible, "binding hides the source");
        assert!(doc.0.layers[0].hidden_as_mask);
        assert!(doc.0.is_mask_source(mask_id));

        // Hidden source still clips: the detail is cut to the small square.
        let normals = doc.0.evaluate();
        // Detail bevel at x 4..12, source coverage starts at x 8: the bevel is
        // kept only inside the bound mask.
        assert!(normals.get(10, 20).x.abs() > 0.001);
        assert!((*normals.get(58, 32) - glam::Vec3::Z).length() < 1e-5);

        apply_layer_action(LayerAction::ToggleMask, &mut state, &mut doc);
        assert!(doc.0.layers[1].mask_layer.is_none());
        assert!(doc.0.layers[0].visible, "unbinding restores visibility");
        assert!(!doc.0.layers[0].hidden_as_mask);
    }

    #[test]
    fn binding_a_layer_above_uses_it_as_the_mask() {
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(64)));
        doc.0.layers.push(rect_layer(
            "mask",
            Vec2::new(8.0, 8.0),
            Vec2::new(40.0, 40.0),
        ));
        doc.0.layers.push(rect_layer(
            "detail",
            Vec2::new(4.0, 4.0),
            Vec2::new(60.0, 60.0),
        ));
        let mut state = EditorState {
            selected: Some(vec![0]),
            ..Default::default()
        };
        apply_layer_action(LayerAction::BindMaskToAbove, &mut state, &mut doc);
        let mask_id = doc.0.layers[0].id;
        assert_eq!(doc.0.layers[1].mask_layer, Some(mask_id));
        assert!(!doc.0.layers[0].visible);
        assert_eq!(state.selected, Some(vec![0]));
    }

    #[test]
    fn new_mask_layer_inserts_below_and_binds_it() {
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(64)));
        doc.0.layers.push(rect_layer(
            "detail",
            Vec2::new(4.0, 4.0),
            Vec2::new(60.0, 60.0),
        ));
        let mut state = EditorState {
            selected: Some(vec![0]),
            ..Default::default()
        };
        apply_layer_action(LayerAction::NewMaskLayer, &mut state, &mut doc);
        assert_eq!(doc.0.layers.len(), 2);
        assert_eq!(doc.0.layers[0].name, "Mask");
        assert!(doc.0.layers[0].mask.is_some());
        let mask_id = doc.0.layers[0].id;
        assert_eq!(doc.0.layers[1].mask_layer, Some(mask_id));
        assert!(!doc.0.layers[0].visible);
        // The clipped layer stays selected so offset/feather are at hand.
        assert_eq!(state.selected, Some(vec![1]));
    }

    #[test]
    fn boolean_group_can_switch_operations_and_bake_to_one_shape() {
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(64)));
        doc.0
            .layers
            .push(rect_layer("a", Vec2::new(8.0, 8.0), Vec2::new(40.0, 40.0)));
        doc.0
            .layers
            .push(rect_layer("b", Vec2::new(24.0, 8.0), Vec2::new(56.0, 40.0)));
        let mut state = EditorState {
            selected: Some(vec![0]),
            multi_selected: vec![vec![1]],
            ..Default::default()
        };

        apply_layer_action(
            LayerAction::VectorCompound(VectorOp::Add),
            &mut state,
            &mut doc,
        );
        assert_eq!(doc.0.layers.len(), 1);
        assert_eq!(doc.0.layers[0].vector_op, Some(VectorOp::Add));

        apply_layer_action(LayerAction::SetBoolean(VectorOp::Xor), &mut state, &mut doc);
        assert_eq!(doc.0.layers[0].vector_op, Some(VectorOp::Xor));

        apply_layer_action(LayerAction::FlattenCompound, &mut state, &mut doc);
        assert_eq!(doc.0.layers.len(), 1);
        assert!(doc.0.layers[0].vector_op.is_none());
        let LayerKind::Shape(shape) = &doc.0.layers[0].kind else {
            panic!("expected shape");
        };
        assert!(matches!(shape.source, ShapeSource::Path(_)));
        // XOR leaves a hole in the middle of the two overlapping rectangles.
        assert!(shape.source.contains(Vec2::new(12.0, 24.0)));
        assert!(!shape.source.contains(Vec2::new(32.0, 24.0)));
        assert!(shape.source.contains(Vec2::new(52.0, 24.0)));
        assert_eq!(state.selected, Some(vec![0]));
    }

    #[test]
    fn outline_stroke_creates_a_new_shape_above_the_source() {
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(64)));
        doc.0.layers.push(rect_layer(
            "a",
            Vec2::new(16.0, 16.0),
            Vec2::new(48.0, 48.0),
        ));
        let mut state = EditorState {
            selected: Some(vec![0]),
            ..Default::default()
        };
        assert!(outline_selected_stroke(&mut state, &mut doc, 6.0));
        assert_eq!(doc.0.layers.len(), 2);
        assert_eq!(state.selected, Some(vec![1]));
        assert!(doc.0.layers[1].name.contains("stroke"));
        let LayerKind::Shape(shape) = &doc.0.layers[1].kind else {
            panic!("expected stroke shape");
        };
        assert!(matches!(shape.source, ShapeSource::Path(_)));
    }

    fn locked_document() -> DocumentRes {
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(64)));
        doc.0.layers.push(rect_layer(
            "locked",
            Vec2::new(20.0, 20.0),
            Vec2::new(24.0, 24.0),
        ));
        doc.0.layers[0].locked = true;
        doc.0.layers.push(rect_layer(
            "free",
            Vec2::new(44.0, 44.0),
            Vec2::new(24.0, 24.0),
        ));
        doc
    }

    #[test]
    fn a_locked_layer_survives_delete() {
        let mut doc = locked_document();
        let mut state = EditorState {
            selected: Some(vec![0]),
            ..Default::default()
        };
        assert!(!delete_selected_layers(&mut state, &mut doc));
        assert_eq!(doc.0.layers.len(), 2, "the locked layer stays");
        assert!(state.status.contains("locked"));
    }

    #[test]
    fn deleting_a_selection_keeps_its_locked_members() {
        let mut doc = locked_document();
        let mut state = EditorState {
            selected: Some(vec![0]),
            multi_selected: vec![vec![1]],
            ..Default::default()
        };
        assert!(delete_selected_layers(&mut state, &mut doc));
        assert_eq!(doc.0.layers.len(), 1);
        assert_eq!(doc.0.layers[0].name, "locked");
        assert!(state.status.contains("locked layer kept"));
    }

    #[test]
    fn the_lock_toggles_the_whole_selection() {
        let mut doc = locked_document();
        doc.0.layers[0].locked = false;
        let mut state = EditorState {
            selected: Some(vec![0]),
            multi_selected: vec![vec![1]],
            ..Default::default()
        };
        toggle_lock_selected(&mut state, &mut doc);
        assert!(doc.0.layers[0].locked && doc.0.layers[1].locked);
        assert!(state.status.contains("Locked 2 layers"));
        assert!(selection_locked(&doc.0, &state));

        // The primary row now reads as locked, so the second toggle unlocks.
        toggle_lock_selected(&mut state, &mut doc);
        assert!(!doc.0.layers[0].locked && !doc.0.layers[1].locked);
        assert!(!selection_locked(&doc.0, &state));
    }

    #[test]
    fn locking_a_group_protects_its_children() {
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(64)));
        let mut group = Layer::group("group", vec![Layer::flat("inside")]);
        group.locked = true;
        doc.0.layers.push(group);
        assert!(path_locked(&doc.0, &[0, 0]));
        assert!(path_locked(&doc.0, &[0]));
    }

    #[test]
    fn a_locked_layer_refuses_geometry_actions() {
        let mut doc = locked_document();
        let mut state = EditorState {
            selected: Some(vec![0]),
            ..Default::default()
        };
        let before = doc.0.layers[0].clone();
        apply_layer_action(LayerAction::OutlineStroke, &mut state, &mut doc);
        assert_eq!(doc.0.layers.len(), 2, "no stroke layer was added");
        assert_eq!(doc.0.layers[0].name, before.name);
        assert!(state.status.contains("locked"));
    }
}
