use bevy::prelude::*;
use bevy::render::view::window::screenshot::{save_to_disk, Screenshot};
use bevy::tasks::{block_on, poll_once, AsyncComputeTaskPool, Task};
use bevy_egui::{egui, EguiPrimaryContextPass};
use glam::{Vec2, Vec3};
use ns_core::brush::Symmetry;
use ns_core::convention::{encode_normal, GreenChannel};
use ns_core::{
    Bounds, Document, EvalCache, Grid, Layer, LayerId, Node, Path, Profile, ShapeSource,
};
use std::collections::{HashMap, HashSet};
use std::ops::{Deref, DerefMut};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::{gpu, preview, ui};

/// Wires the editor resources, the live evaluation loop, and the egui passes.
pub struct NormalStudioPlugin;

impl Plugin for NormalStudioPlugin {
    fn build(&self, app: &mut App) {
        // Matches SURFACE in the UI theme, so nothing flashes a different
        // tone at the window edges before egui paints.
        app.insert_resource(ClearColor(Color::srgb(0.055, 0.078, 0.090)))
            .insert_resource(GlobalAmbientLight {
                color: Color::srgb(0.75, 0.82, 1.0),
                brightness: 220.0,
                ..default()
            })
            // The full-window UI camera owns the primary egui context. Without
            // this, bevy_egui attaches the context to the first camera (the 3D
            // preview) and sizes the whole UI to that camera's viewport.
            .insert_resource(bevy_egui::EguiGlobalSettings {
                auto_create_primary_context: false,
                ..default()
            })
            // Per-pass GPU timestamps for the performance HUD. Bevy only adds
            // this plugin automatically with the `tracing-tracy` feature.
            .add_plugins(bevy::render::diagnostic::RenderDiagnosticsPlugin)
            .init_resource::<EditorState>()
            .init_resource::<EvalRuntime>()
            .insert_non_send(MainThreadOnly)
            .insert_resource(EvalCacheRes(EvalCache::new()))
            .init_resource::<crate::autosave::AutosaveRuntime>()
            .add_systems(Startup, preview::setup_scene)
            // Fonts must be in place before the first egui pass: `set_fonts`
            // only applies on the next pass, and a named family used before it
            // exists panics inside epaint.
            .add_systems(
                PreUpdate,
                install_ui_fonts.before(bevy_egui::run_egui_context_pass_loop_system),
            )
            .add_systems(
                Update,
                (
                    evaluate_when_dirty,
                    preview::auto_rotate,
                    preview::apply_preview_style,
                    preview::sync_preview_uv_transform,
                    preview::sync_preview_height.after(crate::pbr_ui::sync_pbr_material),
                    preview::sync_preview_tessellation,
                    preview::sync_preview_visibility,
                    capture_when_requested,
                    sync_window_title,
                    apply_window_icon,
                    crate::pen_input::install_pen_input,
                    crate::autosave::autosave_system,
                ),
            )
            .add_systems(
                EguiPrimaryContextPass,
                (ui::editor_ui, preview::apply_camera_viewport).chain(),
            );
    }
}

/// Installs the bundled Inter / JetBrains faces the first time the primary
/// egui context exists, which is before any UI system runs.
fn install_ui_fonts(mut done: Local<bool>, mut contexts: Query<&mut bevy_egui::EguiContext>) {
    if *done {
        return;
    }
    for mut context in &mut contexts {
        ui::install_fonts(context.get_mut());
        *done = true;
    }
}

#[derive(Resource)]
pub struct DocumentRes(pub SharedDocument);

/// Applies the launcher artwork to the OS window so the taskbar button matches
/// the EXE icon. winit otherwise registers the generic application icon on
/// Windows, which is what the taskbar would show.
/// Marker that forces a system onto the main thread. Bevy 0.19 stores the
/// `winit` windows in a thread-local instead of a non-send resource.
#[derive(Default)]
pub(crate) struct MainThreadOnly;

/// Keep the OS-level icon in sync with `assets/icon.png`.
///
/// `winit` exposes window icon changes only on the main thread, and Bevy 0.19
/// keeps its windows in `WINIT_WINDOWS` rather than a `WinitWindows` resource.
fn apply_window_icon(
    _main_thread: NonSend<MainThreadOnly>,
    primary: Query<Entity, With<bevy::window::PrimaryWindow>>,
    mut applied: Local<bool>,
) {
    if *applied {
        return;
    }
    let Ok(entity) = primary.single() else {
        return;
    };
    let bytes = include_bytes!("../assets/icon.png");
    let Ok(image) = image::load_from_memory(bytes) else {
        return;
    };
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    let Ok(icon) = winit::window::Icon::from_rgba(rgba.into_raw(), width, height) else {
        return;
    };
    let set = bevy::winit::WINIT_WINDOWS.with_borrow(|windows| {
        let Some(window) = windows.get_window(entity) else {
            return false;
        };
        window.set_window_icon(Some(icon));
        true
    });
    if set {
        *applied = true;
    }
}

impl DocumentRes {
    pub fn new(doc: impl Into<SharedDocument>) -> Self {
        Self(doc.into())
    }
}

/// Copy-on-write document handle.
///
/// Cloning is O(1): the evaluation job and the undo stack can keep reading the
/// previous revision while the editor mutates the live one. The first mutable
/// access calls `Arc::make_mut`, which deep-clones only then. This keeps
/// selection-only clicks and background evaluations from cloning every grid.
#[derive(Clone, Debug)]
pub struct SharedDocument(Arc<Document>);

impl SharedDocument {
    pub fn new(doc: Document) -> Self {
        Self(Arc::new(doc))
    }

    /// True when both handles still point at the same document allocation.
    #[cfg(test)]
    pub fn shares_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// Allocation identity, used to avoid counting shared snapshots twice.
    pub fn ptr(&self) -> *const Document {
        Arc::as_ptr(&self.0)
    }
}

impl From<Document> for SharedDocument {
    fn from(doc: Document) -> Self {
        Self::new(doc)
    }
}

impl Deref for SharedDocument {
    type Target = Document;

    fn deref(&self) -> &Document {
        &self.0
    }
}

impl DerefMut for SharedDocument {
    fn deref_mut(&mut self) -> &mut Document {
        Arc::make_mut(&mut self.0)
    }
}

/// Optional automation hook: capture the window and exit.
#[derive(Resource)]
pub struct ScreenshotRequest {
    pub path: PathBuf,
    pub frame: u32,
}

/// Keep the OS title bar in sync with the open project and its unsaved marker.
fn sync_window_title(
    state: Res<EditorState>,
    mut windows: Query<&mut Window, With<bevy::window::PrimaryWindow>>,
) {
    let Ok(mut window) = windows.single_mut() else {
        return;
    };
    let title = if state.project_path.is_some() || state.has_unsaved_changes() {
        let name = state
            .project_name()
            .unwrap_or_else(|| "Untitled".to_string());
        let marker = if state.has_unsaved_changes() {
            " •"
        } else {
            ""
        };
        format!("{name}{marker} — N-shade")
    } else {
        "N-shade — Normal Map Studio".to_string()
    };
    if window.title != title {
        window.title = title;
    }
}

fn capture_when_requested(
    mut commands: Commands,
    request: Option<ResMut<ScreenshotRequest>>,
    mut exit: MessageWriter<AppExit>,
) {
    let Some(mut request) = request else {
        return;
    };
    request.frame += 1;
    if request.frame == 180 {
        commands
            .spawn(Screenshot::primary_window())
            .observe(save_to_disk(request.path.clone()));
    }
    if request.frame == 240 {
        exit.write(AppExit::Success);
    }
}

/// Bevy wrapper; `EvalCache` lives in `ns_core` and cannot implement `Resource`
/// directly because of the orphan rule.
#[derive(Resource)]
pub struct EvalCacheRes(pub EvalCache);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ViewportTab {
    #[default]
    ThreeD,
    TwoD,
}

/// A running 3D <-> 2D cross-fade. The target is always `viewport_tab`; only
/// where it came from and when it started need remembering.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewTransition {
    pub from: ViewportTab,
    pub started: f64,
}

impl ViewportTab {
    pub fn label(self) -> &'static str {
        match self {
            Self::ThreeD => "3D View",
            Self::TwoD => "2D Canvas",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RightTab {
    #[default]
    Properties,
    Mesh,
    Bake,
    Lightmap,
    Pbr,
}

impl RightTab {
    pub const ALL: [Self; 5] = [
        Self::Properties,
        Self::Mesh,
        Self::Bake,
        Self::Lightmap,
        Self::Pbr,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Properties => "Properties",
            Self::Mesh => "Mesh",
            Self::Bake => "Bake",
            Self::Lightmap => "Lightmap",
            Self::Pbr => "PBR",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CanvasTool {
    #[default]
    Select,
    Rectangle,
    Ellipse,
    Star,
    /// Click-to-place polygon: corners close into a filled shape.
    Polygon,
    /// Click-to-place cable: a smooth round tube with shaped end caps.
    Cable,
    /// Click-to-place live text, edited in the Properties panel.
    Text,
    Ruler,
    /// Rectangular cleanup lasso.
    LassoRect,
    /// Elliptical cleanup lasso.
    LassoEllipse,
    /// Polygon cleanup lasso (click points, Enter closes).
    LassoPolygon,
    /// Freehand cleanup lasso.
    LassoFree,
    /// Straight-line knife: drag a line to cut through the selected shapes.
    LassoLine,
    Pen,
    Node,
    AddAnchor,
    DeleteAnchor,
    ConvertAnchor,
    Paint,
}

impl CanvasTool {
    pub const VECTOR_TOOLS: [Self; 6] = [
        Self::Pen,
        Self::Cable,
        Self::Node,
        Self::AddAnchor,
        Self::DeleteAnchor,
        Self::ConvertAnchor,
    ];

    /// Lasso variants behind the single rail tile.
    pub const LASSO_TOOLS: [Self; 5] = [
        Self::LassoRect,
        Self::LassoEllipse,
        Self::LassoPolygon,
        Self::LassoFree,
        Self::LassoLine,
    ];

    /// Shape primitives behind the single rail tile.
    pub const SHAPE_TOOLS: [Self; 4] = [Self::Rectangle, Self::Ellipse, Self::Star, Self::Polygon];

    pub fn is_vector_tool(self) -> bool {
        matches!(
            self,
            Self::Pen
                | Self::Cable
                | Self::Node
                | Self::AddAnchor
                | Self::DeleteAnchor
                | Self::ConvertAnchor
        )
    }

    /// True for every primitive shape tool; the rail shows one tile for all of
    /// them and right-click picks which one the tile arms.
    pub fn is_shape_tool(self) -> bool {
        matches!(
            self,
            Self::Rectangle | Self::Ellipse | Self::Star | Self::Polygon
        )
    }

    /// True for every lasso variant; the rail shows one tile for all of them.
    pub fn is_lasso_tool(self) -> bool {
        matches!(
            self,
            Self::LassoRect
                | Self::LassoEllipse
                | Self::LassoPolygon
                | Self::LassoFree
                | Self::LassoLine
        )
    }

    pub fn is_node_edit_tool(self) -> bool {
        matches!(
            self,
            Self::Node | Self::AddAnchor | Self::DeleteAnchor | Self::ConvertAnchor
        )
    }
}

impl CanvasTool {
    pub fn label(self) -> &'static str {
        match self {
            Self::Select => "Select",
            Self::Rectangle => "Rectangle",
            Self::Ellipse => "Ellipse",
            Self::Star => "Star",
            Self::Polygon => "Polygon",
            Self::Cable => "Cable",
            Self::Text => "Text",
            Self::Ruler => "Ruler",
            Self::LassoRect => "Rectangular Lasso",
            Self::LassoEllipse => "Elliptical Lasso",
            Self::LassoPolygon => "Polygon Lasso",
            Self::LassoFree => "Freehand Lasso",
            Self::LassoLine => "Line Lasso",
            Self::Pen => "Pen",
            Self::Node => "Node",
            Self::AddAnchor => "Add Anchor",
            Self::DeleteAnchor => "Delete Anchor",
            Self::ConvertAnchor => "Convert Point",
            Self::Paint => "Paint",
        }
    }
}

pub use ns_core::CableCap;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PaintMode {
    #[default]
    Raise,
    Lower,
    Smooth,
    Flatten,
}

impl PaintMode {
    pub const ALL: [Self; 4] = [Self::Raise, Self::Lower, Self::Smooth, Self::Flatten];

    pub fn label(self) -> &'static str {
        match self {
            Self::Raise => "Raise",
            Self::Lower => "Lower",
            Self::Smooth => "Smooth",
            Self::Flatten => "Flatten",
        }
    }
}

/// Layer manipulation offered inside the 3D preview.
///
/// Layers are UV-mapped 2D content, so these edit the layer's document-space
/// placement; the 2D canvas reflects the same change.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PreviewGizmo {
    #[default]
    Off,
    Move,
    Stretch,
}

impl PreviewGizmo {
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Move => "Move",
            Self::Stretch => "Stretch",
        }
    }
}

/// Cache key for the mask overlay: layer id, layer revision, mask revision,
/// clipping-source revision, and document size.
pub type MaskOverlayKey = (LayerId, u64, u64, u64, u32, u32);

/// What the 2D canvas shows while a mask is open.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MaskView {
    /// The live normal map with hidden areas tinted, like a quick mask.
    #[default]
    Overlay,
    /// The painted mask in grayscale.
    Mask,
    /// The live normal map only, to judge the blend.
    Result,
}

impl MaskView {
    pub const ALL: [Self; 3] = [Self::Overlay, Self::Mask, Self::Result];

    pub fn label(self) -> &'static str {
        match self {
            Self::Overlay => "Overlay",
            Self::Mask => "Mask",
            Self::Result => "Result",
        }
    }

    /// The Mask view covers the normal map, so evaluation can wait.
    pub fn shows_result(self) -> bool {
        !matches!(self, Self::Mask)
    }
}

/// Appearance preset for imported photos converted to height/normal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PhotoPreset {
    #[default]
    Stone,
    Metal,
    Wood,
    Fabric,
    Concrete,
    Alpha,
}

impl PhotoPreset {
    pub const ALL: [Self; 6] = [
        Self::Stone,
        Self::Metal,
        Self::Wood,
        Self::Fabric,
        Self::Concrete,
        Self::Alpha,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Stone => "Stone",
            Self::Metal => "Metal",
            Self::Wood => "Wood",
            Self::Fabric => "Fabric",
            Self::Concrete => "Concrete",
            Self::Alpha => "Alpha",
        }
    }

    /// Strength plus three blur radii/weights. Radii are authored at 1024 px
    /// and scale with the photo resolution.
    pub fn params(self, scale: f32) -> (f32, [f32; 3], [f32; 3]) {
        let s = scale.max(0.05);
        match self {
            Self::Stone => (6.0, [2.0 * s, 8.0 * s, 24.0 * s], [1.0, 0.7, 0.35]),
            Self::Metal => (10.0, [1.0 * s, 4.0 * s, 14.0 * s], [1.0, 0.8, 0.3]),
            Self::Wood => (5.0, [3.0 * s, 12.0 * s, 36.0 * s], [1.0, 0.5, 0.25]),
            Self::Fabric => (3.5, [2.0 * s, 6.0 * s, 16.0 * s], [0.8, 1.0, 0.45]),
            Self::Concrete => (7.0, [2.0 * s, 10.0 * s, 28.0 * s], [1.0, 0.8, 0.45]),
            Self::Alpha => (1.0, [0.0; 3], [1.0, 0.0, 0.0]),
        }
    }
}

/// Live state for the floating custom-bevel curve editor.
pub struct CurveEditor {
    pub targets: Vec<LayerId>,
    /// When set, edits this path edge instead of the shapes' own profiles.
    pub edge: Option<EdgeRef>,
    /// Normalized control points, `(t, value)` in `0..=1`.
    pub points: Vec<egui::Pos2>,
    pub initial: Vec<egui::Pos2>,
    pub drag: Option<usize>,
    /// Highlighted point shown in the value readout.
    pub selected: Option<usize>,
    /// Profiles captured when the editor opened, used by Cancel.
    pub original: Vec<(LayerId, Profile)>,
    /// Original per-edge override when `edge` is set, used by Cancel.
    pub original_edge: Option<(LayerId, EdgeRef, Option<Profile>)>,
}

#[derive(Clone)]
pub struct UndoSnapshot {
    pub doc: SharedDocument,
    pub selected: Option<Vec<usize>>,
    pub multi_selected: Vec<Vec<usize>>,
    pub collapsed: HashSet<LayerId>,
}

impl UndoSnapshot {
    /// Metadata held by the snapshot, excluding the shared document.
    fn meta_bytes(&self) -> usize {
        let path_bytes = self
            .selected
            .iter()
            .chain(self.multi_selected.iter())
            .map(|path| path.capacity() * std::mem::size_of::<usize>())
            .sum::<usize>();
        path_bytes + self.collapsed.capacity() * std::mem::size_of::<LayerId>()
    }
}

/// Memory held by a snapshot stack, counting each shared document allocation
/// once. Consecutive snapshots can share a `SharedDocument` after undo/redo
/// bookkeeping, and counting those twice would evict steps too early.
fn stack_bytes(stack: &[UndoSnapshot]) -> usize {
    let mut seen = HashSet::new();
    let mut bytes = 0usize;
    for snapshot in stack {
        bytes = bytes.saturating_add(snapshot.meta_bytes());
        if seen.insert(snapshot.doc.ptr()) {
            bytes = bytes.saturating_add(snapshot.doc.estimated_bytes());
        }
    }
    bytes
}

/// Enforce the depth and memory limits on a snapshot stack, always keeping the
/// newest entry so the most recent step stays undoable.
fn trim_snapshots(stack: &mut Vec<UndoSnapshot>, budget: usize) {
    while stack.len() > UNDO_LIMIT {
        stack.remove(0);
    }
    while stack.len() > 1 && stack_bytes(stack) > budget {
        stack.remove(0);
    }
}

const UNDO_LIMIT: usize = 24;

/// Upper bound on the memory held by the undo and redo stacks. The depth limit
/// still applies; this only evicts the oldest entries earlier on very large
/// documents, where 24 full snapshots can reach gigabytes.
const UNDO_MEMORY_BUDGET: usize = 512 * 1024 * 1024;

/// What a smart guide constrains while the pen is placing a node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuideKind {
    /// Straight onto another vertex.
    Vertex,
    /// Same Y as another vertex.
    Horizontal,
    /// Same X as another vertex.
    Vertical,
    /// On a line parallel to an existing edge.
    Parallel,
    /// On a line perpendicular to an existing edge.
    Perpendicular,
    /// Aligned with the bounding-box edge of another shape.
    Edge,
    /// Aligned with the center of another shape.
    Center,
    /// Aligned with a canvas edge or midline.
    Canvas,
}

impl GuideKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Vertex => "Vertex",
            Self::Horizontal => "Horizontal",
            Self::Vertical => "Vertical",
            Self::Parallel => "Parallel",
            Self::Perpendicular => "Perpendicular",
            Self::Edge => "Edge",
            Self::Center => "Center",
            Self::Canvas => "Canvas",
        }
    }
}

/// One alignment guide the pen is honouring for the pending node.
///
/// Authoring-only: the guide is rebuilt from the visible shapes on every
/// pointer move and never becomes part of the document.
#[derive(Clone, Debug)]
pub struct SmartGuide {
    pub kind: GuideKind,
    /// Point the guide line runs through, in document space.
    pub anchor: Vec2,
    /// Unit direction of the guide line; zero for a vertex snap.
    pub direction: Vec2,
    /// Edge that inspired a parallel/perpendicular guide, highlighted on canvas.
    pub segment: Option<(Vec2, Vec2)>,
}

/// In-progress custom vector outline drawn with the pen tool.
#[derive(Clone, Debug, Default)]
pub struct VectorDraft {
    pub nodes: Vec<Node>,
    pub hover: Option<Vec2>,
    pub drag_index: Option<usize>,
    /// The pointer is close enough to the first node that a click closes the
    /// loop, so the ring around that node lights up.
    pub close_armed: bool,
    /// Alignment guides the pending node is snapped to, rebuilt on every hover.
    pub guides: Vec<SmartGuide>,
}

/// In-progress lasso region. Rect and ellipse keep two drag corners in
/// `points`; polygon and freehand accumulate vertices.
#[derive(Clone, Debug, Default)]
pub struct LassoDraft {
    pub points: Vec<Vec2>,
    pub hover: Option<Vec2>,
}

/// What a committed lasso does to shape layers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LassoMode {
    /// Subtract the region from shape outlines so the bevel is rebuilt along
    /// the cut, and split the remainder into editable pieces.
    #[default]
    TrimShape,
    /// Hide the region through painted masks (the original behavior).
    ErasePixels,
}

impl LassoMode {
    pub const ALL: [Self; 2] = [Self::TrimShape, Self::ErasePixels];

    pub fn label(self) -> &'static str {
        match self {
            Self::TrimShape => "Trim shape",
            Self::ErasePixels => "Erase pixels",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::TrimShape => {
                "Cuts shape outlines and rebuilds the bevel; drag right/down to cut, left/up to divide"
            }
            Self::ErasePixels => "Hides the region through painted masks, vectors stay untouched",
        }
    }
}

/// Ruler guide that straight and angled drawing snaps to.
///
/// The guide is an infinite line through `origin`; drawing tools project onto
/// it while Shift is held, the way a ruler guides a pen on paper.
#[derive(Clone, Copy, Debug)]
pub struct RulerState {
    /// Anchor in document space.
    pub origin: Vec2,
    /// Direction in radians from +X, in document space (Y points down).
    pub angle: f32,
}

impl RulerState {
    /// Unit vector along the ruler.
    pub fn direction(self) -> Vec2 {
        Vec2::from_angle(self.angle)
    }

    /// Angle in degrees wrapped to 0..360, for display and editing.
    pub fn degrees(self) -> f32 {
        self.angle.to_degrees().rem_euclid(360.0)
    }
}

/// A node inside one of a path's subpaths.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NodeRef {
    pub subpath: usize,
    pub node: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EdgeRef {
    pub subpath: usize,
    /// Index of the node that starts the edge.
    pub edge: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeDragKind {
    Move,
    PullHandles,
    HandleIn,
    HandleOut,
}

/// Handle under the pointer in the 3D sweep-path editor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SweepPathHover {
    pub node: NodeRef,
    pub kind: NodeDragKind,
}

/// Path-handle drag in progress in the 3D viewport.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SweepPathDrag {
    /// Path-guide layer being edited.
    pub layer_id: LayerId,
    pub hover: SweepPathHover,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResizeHandle {
    N,
    S,
    E,
    W,
    Ne,
    Nw,
    Se,
    Sw,
}

impl ResizeHandle {
    pub fn moves_left(self) -> bool {
        matches!(self, Self::W | Self::Nw | Self::Sw)
    }

    pub fn moves_right(self) -> bool {
        matches!(self, Self::E | Self::Ne | Self::Se)
    }

    pub fn moves_top(self) -> bool {
        matches!(self, Self::N | Self::Ne | Self::Nw)
    }

    pub fn moves_bottom(self) -> bool {
        matches!(self, Self::S | Self::Se | Self::Sw)
    }
}

/// How the exported height map is produced.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HeightSource {
    /// Direct layer heights unless the stack contains imported normal maps,
    /// which have no analytic height and still need integration.
    Auto,
    /// Composite the layer stack's height fields (flat plateaus, equal depths).
    Direct,
    /// Rebuild height from the final normal field (the legacy behavior).
    #[default]
    Integrated,
}

impl HeightSource {
    pub const ALL: [Self; 3] = [Self::Auto, Self::Direct, Self::Integrated];

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Direct => "Direct (layers)",
            Self::Integrated => "Integrated (normals)",
        }
    }

    /// One-line explanation shown under the export control.
    pub fn hint(self) -> &'static str {
        match self {
            Self::Auto => {
                "Direct layer heights; integrates only when the stack contains imported normals."
            }
            Self::Direct => {
                "Composite layer heights: flat plateaus and equal depths. Overlaps may differ from the normal map."
            }
            Self::Integrated => {
                "Rebuild from the final normal map: matches the normals, but can drift and ring at corners."
            }
        }
    }

    /// Whether this source uses integration for the given document.
    ///
    /// Auto is an interim scaffold: imported normal layers still integrate the
    /// final field until per-layer integration lands.
    pub fn integrates_height(self, has_imported_normals: bool) -> bool {
        match self {
            Self::Auto => has_imported_normals,
            Self::Direct => false,
            Self::Integrated => true,
        }
    }
}

/// In-progress canvas gesture. Geometry snapshots make drags non-cumulative.
#[derive(Clone, Debug)]
pub enum CanvasDrag {
    Create {
        tool: CanvasTool,
        start: Vec2,
        current: Vec2,
    },
    Move {
        layer_id: LayerId,
        start: Vec2,
        source: ShapeSource,
    },
    MoveGroup {
        start: Vec2,
        sources: Vec<(LayerId, ShapeSource)>,
        /// When set, the drag is constrained to this document-space angle.
        /// Duplicate drags use the ruler direction; group/multi moves leave it
        /// open so smart guides keep working.
        axis: Option<f32>,
    },
    Rotate {
        center: Vec2,
        start_angle: f32,
        sources: Vec<(LayerId, ShapeSource)>,
    },
    Paint {
        layer_id: LayerId,
        last: Vec2,
    },
    MaskPaint {
        layer_id: LayerId,
        last: Vec2,
    },
    /// Scale a shape, group or multi-selection around one union bounding box.
    /// `sources` holds the pre-gesture geometry of every descendant shape so a
    /// drag is non-cumulative even when groups and children are both selected.
    Resize {
        handle: ResizeHandle,
        sources: Vec<(LayerId, ShapeSource)>,
        bounds: Bounds,
    },
    NodeEdit {
        layer_id: LayerId,
        original: Path,
        anchor: Vec2,
        nodes: Vec<NodeRef>,
        kind: NodeDragKind,
    },
}

/// The 3D preview mesh whose material displays the evaluated normal map.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Component)]
pub enum PreviewShape {
    #[default]
    Cube,
    RoundedCube,
    Plane,
    Cylinder,
    Imported,
}

impl PreviewShape {
    pub const BUILTIN: [Self; 4] = [Self::Cube, Self::RoundedCube, Self::Plane, Self::Cylinder];

    pub fn label(self) -> &'static str {
        match self {
            Self::Cube => "Cube",
            Self::RoundedCube => "Rounded Cube",
            Self::Plane => "Plane",
            Self::Cylinder => "Cylinder",
            Self::Imported => "Imported",
        }
    }
}

/// How the 3D preview applies the evaluated height map to the surface.
/// `Off` keeps a pure normal-mapped surface; the other modes use Bevy's
/// parallax mapping with the document height texture as the depth map.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PreviewParallax {
    Off,
    #[default]
    Relief,
    Occlusion,
}

impl PreviewParallax {
    pub const ALL: [Self; 3] = [Self::Off, Self::Relief, Self::Occlusion];

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Relief => "Relief",
            Self::Occlusion => "Occlusion",
        }
    }
}

/// Subdivision level of the built-in 3D preview meshes. Tessellation does not
/// change the document; it only rebuilds the cube, rounded cube, cylinder and
/// plane at a higher vertex density so silhouettes and grazing-angle shading
/// stay smooth.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PreviewTessellation {
    Off,
    Low,
    #[default]
    Medium,
    High,
}

impl PreviewTessellation {
    pub const ALL: [Self; 4] = [Self::Off, Self::Low, Self::Medium, Self::High];

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
        }
    }

    /// Face subdivisions for the rounded cube.
    pub fn rounded_segments(self) -> u32 {
        match self {
            Self::Off => ROUNDED_CUBE_RESOLUTION_BASE,
            Self::Low => 64,
            Self::Medium => 96,
            Self::High => 128,
        }
    }

    /// Quads per edge for the flat cube and the plane.
    pub fn edge_segments(self) -> u32 {
        match self {
            Self::Off => 1,
            Self::Low => 8,
            Self::Medium => 24,
            Self::High => 48,
        }
    }

    /// Radial segments for the cylinder.
    pub fn cylinder_segments(self) -> u32 {
        match self {
            Self::Off => 64,
            Self::Low => 96,
            Self::Medium => 128,
            Self::High => 192,
        }
    }

    /// Extra edge loops for the plane; `0` keeps the two-triangle quad.
    pub fn plane_subdivisions(self) -> u32 {
        match self {
            Self::Off => 0,
            Self::Low => 16,
            Self::Medium => 48,
            Self::High => 96,
        }
    }
}

/// Base rounded-cube resolution. Kept in this module so the tessellation menu
/// and the mesh builder agree.
pub const ROUNDED_CUBE_RESOLUTION_BASE: u32 = 48;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MaterialPreset {
    #[default]
    MatteGray,
    Clay,
    Steel,
}

impl MaterialPreset {
    pub const ALL: [Self; 3] = [Self::MatteGray, Self::Clay, Self::Steel];

    pub fn label(self) -> &'static str {
        match self {
            Self::MatteGray => "Matte Gray",
            Self::Clay => "Clay",
            Self::Steel => "Steel",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LightingPreset {
    #[default]
    Studio,
    Soft,
    Dramatic,
}

impl LightingPreset {
    pub const ALL: [Self; 3] = [Self::Studio, Self::Soft, Self::Dramatic];

    pub fn label(self) -> &'static str {
        match self {
            Self::Studio => "Studio",
            Self::Soft => "Soft",
            Self::Dramatic => "Dramatic",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EnvironmentPreset {
    #[default]
    Neutral,
    Cool,
    Warm,
}

impl EnvironmentPreset {
    pub const ALL: [Self; 3] = [Self::Neutral, Self::Cool, Self::Warm];

    pub fn label(self) -> &'static str {
        match self {
            Self::Neutral => "Neutral",
            Self::Cool => "Cool",
            Self::Warm => "Warm",
        }
    }
}

#[derive(Resource)]
pub struct PreviewHandles {
    pub material: Handle<StandardMaterial>,
    /// Neutral white material used by the optional lightmap lighting preview.
    pub lightmap_material: Handle<StandardMaterial>,
    pub normal_texture: Handle<Image>,
    pub height_texture: Handle<Image>,
    /// Built-in preview meshes, rebuilt when the tessellation level changes.
    pub meshes: PreviewMeshHandles,
}

#[derive(Clone, Default)]
pub struct PreviewMeshHandles {
    pub cube: Handle<Mesh>,
    pub rounded_cube: Handle<Mesh>,
    pub plane: Handle<Mesh>,
    pub cylinder: Handle<Mesh>,
}

/// Default corner rounding of a drawn rectangle, as a fraction of its shorter
/// side: rounded but clearly not a pill.
pub(crate) const DEFAULT_RECT_CORNER: f32 = 0.12;

/// A short rolling window of timings for the performance HUD.
#[derive(Clone, Debug, Default)]
pub struct PerfStat {
    samples: Vec<f32>,
}

impl PerfStat {
    const CAPACITY: usize = 120;

    pub fn push(&mut self, value: f32) {
        if !value.is_finite() {
            return;
        }
        if self.samples.len() == Self::CAPACITY {
            self.samples.remove(0);
        }
        self.samples.push(value);
    }

    pub fn last(&self) -> Option<f32> {
        self.samples.last().copied()
    }

    pub fn mean(&self) -> Option<f32> {
        (!self.samples.is_empty())
            .then(|| self.samples.iter().sum::<f32>() / self.samples.len() as f32)
    }

    pub fn p95(&self) -> Option<f32> {
        if self.samples.is_empty() {
            return None;
        }
        let mut sorted = self.samples.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let index = ((sorted.len() - 1) as f32 * 0.95).round() as usize;
        sorted.get(index).copied()
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }
}

#[derive(Resource)]
pub struct EditorState {
    /// Path of child indices from the root layer list.
    pub selected: Option<Vec<usize>>,
    /// Additional sibling paths selected with Ctrl/Cmd for grouping.
    pub multi_selected: Vec<Vec<usize>>,
    /// Internal layer clipboard. Stored as original-id clones so every paste
    /// can generate fresh ids and remap references between the pasted layers.
    pub layer_clipboard: Vec<Layer>,
    /// Groups whose children are hidden in the layer list.
    pub collapsed: HashSet<LayerId>,
    pub show_height: bool,
    pub dirty: bool,
    pub eval_timer: Timer,
    pub zoom: f32,
    pub pan: egui::Vec2,
    pub preview_rect: Option<egui::Rect>,
    pub preview_shape: PreviewShape,
    /// Height-map technique used by the 3D preview (see [`PreviewParallax`]).
    pub preview_parallax: PreviewParallax,
    /// Multiplies the document height scale when setting the preview's
    /// parallax depth. `0.0` flattens the surface.
    pub preview_height_scale: f32,
    /// Vertex density of the built-in preview meshes.
    pub preview_tessellation: PreviewTessellation,
    pub auto_rotate: bool,
    pub orbit_yaw: f32,
    pub orbit_pitch: f32,
    pub orbit_distance: f32,
    pub tiling: f32,
    pub tex_dims: [usize; 2],
    pub normal_tex_id: Option<egui::TextureId>,
    pub height_tex_id: Option<egui::TextureId>,
    pub using_gpu: bool,
    /// Why the last GPU scene build was rejected. Empty while the GPU path is
    /// active; filled with every detected reason after a fallback.
    pub gate_rejects: Vec<gpu::GateReject>,
    /// Show the floating performance HUD.
    pub show_perf_hud: bool,
    /// Rolling timings for the HUD. These are UI-only and never serialized.
    pub perf_scene_build_ms: PerfStat,
    pub perf_cpu_eval_ms: PerfStat,
    /// Derive the height map by integrating the evaluated normals instead of
    /// compositing the analytic shape heights.
    pub integrate_height: bool,
    /// Development view: draw every icon at several sizes (`--icon-sheet`).
    pub show_icon_sheet: bool,
    /// Move/stretch the selected layer by dragging in the 3D view.
    pub preview_gizmo: PreviewGizmo,
    /// Show draggable sun and scene-light direction handles over the 3D view.
    pub show_light_gizmos: bool,
    /// Show and edit the active sweep operation's path directly on the model.
    pub sweep_path_edit: bool,
    /// Path node or handle currently grabbed in the 3D viewport.
    pub sweep_path_drag: Option<SweepPathDrag>,
    /// Path handle under the pointer, for the highlight and cursor.
    pub sweep_path_hover: Option<SweepPathHover>,
    /// Last clicked path handle, kept highlighted between drags.
    pub sweep_path_selected: Option<SweepPathHover>,
    /// Precision readout for the last surface hit (document px and UV).
    pub sweep_path_readout: String,
    /// Draw the selected layer's mask over the 2D normal map.
    pub show_mask_overlay: bool,
    /// Opacity of the mask overlay tint, 0..=1.
    pub mask_overlay_opacity: f32,
    /// Cached mask grid for the overlay, keyed by layer revisions.
    pub mask_overlay_key: Option<MaskOverlayKey>,
    pub mask_overlay_grid: Option<Grid<f32>>,
    /// Uploaded overlay texture and the key it was built from.
    pub mask_overlay_tex_key: Option<MaskOverlayKey>,
    pub mask_overlay_tex: Option<egui::TextureHandle>,
    /// Layer whose painted mask is open in the 2D mask canvas.
    pub mask_edit_layer: Option<LayerId>,
    /// Lazily uploaded grayscale preview of the mask being edited.
    pub mask_tex: Option<egui::TextureHandle>,
    /// `(layer, mask revision, width, height, view)` of the data in `mask_tex`.
    pub mask_tex_key: Option<(LayerId, u64, u32, u32, MaskView)>,
    pub mask_view: MaskView,
    /// Opacity-capped buffer of the mask stroke in progress.
    pub mask_stroke: Option<ns_core::brush::MaskStroke>,
    /// Open custom bevel profile window and its targets.
    pub curve_editor: Option<CurveEditor>,
    pub last_eval_ms: f32,
    pub status: String,
    /// Project file currently open, if any. `None` means the document has not
    /// been saved yet.
    pub project_path: Option<PathBuf>,
    /// Folder used by the next Open/Save dialog.
    pub project_dir: Option<PathBuf>,
    /// `edit_serial` at the last save or open. A mismatch means unsaved edits.
    pub project_saved_serial: u64,
    /// A `.nsproj` dropped on the window, loaded by the next UI pass where the
    /// document and its cache are available.
    pub project_open_request: Option<PathBuf>,
    /// Lightmap data restored from a v2 project, applied once the lightmap
    /// runtime is available.
    pub pending_lightmap: Option<ns_core::project::LightmapSaveData>,
    /// Materialized lightmap environment from a v2 project.
    pub pending_lightmap_environment: Option<PathBuf>,
    /// Viewer lightmap-preview flag restored from a v2 project.
    pub pending_lightmap_preview: Option<bool>,
    /// High-to-low bake inputs restored from a v2 project.
    pub pending_bake: Option<ns_core::project::BakeSaveData>,
    pub pending_bake_high: Option<PathBuf>,
    /// Write recovery snapshots of unsaved work.
    pub autosave_enabled: bool,
    /// Recovery snapshots kept per project. Older snapshots rotate out.
    pub autosave_backups: usize,
    /// Scripted runs (screenshots, demo captures, stress tests) must not write
    /// recovery files into the user's folders.
    pub suppress_autosave: bool,
    /// File name of the last completed snapshot, shown in Settings.
    pub autosave_status: String,
    /// Last autosave failure; cleared by the next successful snapshot.
    pub autosave_error: Option<String>,
    pub canvas_tool: CanvasTool,
    /// Request focus for the selected text layer's content field on the next
    /// Properties pass. Set right after the Text tool places a new layer.
    pub focus_text_content: bool,
    /// Start one undo baseline on the frame after placement, so the first
    /// keystroke in the auto-focused Content field is undoable on its own
    /// instead of undoing the whole placement.
    pub text_edit_baseline: bool,
    /// Corner rounding of a newly drawn rectangle as a fraction of its shorter
    /// side: `0.0` draws pointy corners, `0.5` draws a pill. The Properties
    /// corner slider writes back here, so the next rectangle inherits it.
    pub rect_corner: f32,
    /// Cable tool: tube width and height in document pixels.
    pub cable_width: f32,
    pub cable_height: f32,
    /// Terminal shape at both cable ends and its size relative to the width.
    pub cable_cap: CableCap,
    pub cable_cap_scale: f32,
    /// Last vector tool chosen from the Pen flyout; the rail shows this icon.
    pub vector_tool: CanvasTool,
    /// Last shape chosen from the Shapes flyout; the rail tile runs this one.
    pub shape_tool: CanvasTool,
    pub canvas_drag: Option<CanvasDrag>,
    /// Smart-guide targets and sticky values for the active drag.
    pub(crate) snap_session: Option<crate::snapping::SnapSession>,
    /// Guides to draw for the active drag; transient and never saved.
    pub(crate) active_guides: Vec<SmartGuide>,
    pub vector_draft: Option<VectorDraft>,
    /// True while arrow keys are held for a burst of nudges. The first nudge
    /// of a burst takes the undo checkpoint, so holding a key leaves one entry.
    pub nudge_burst: bool,
    /// Last lasso chosen from the flyout; the rail tile runs this one.
    pub lasso_tool: CanvasTool,
    /// What a committed lasso does to shape layers.
    pub lasso_mode: LassoMode,
    /// Region being drawn with a lasso tool.
    pub lasso_draft: Option<LassoDraft>,
    /// The uncommitted marquee is being drawn backwards (right→left or
    /// bottom→top), so committing splits the shape into the region and the
    /// remainder instead of trimming the region away.
    pub lasso_divide: bool,
    /// Optional procedural sci-fi texture generator window.
    pub scifi: crate::scifi::ScifiPanel,
    /// Ruler guide used by Shift snapping while drawing.
    pub ruler: Option<RulerState>,
    /// Alt-dragged duplicates follow the ruler angle instead of free smart
    /// guides. The ruler options bar exposes this as "Lock dupes".
    pub ruler_locks_duplicates: bool,
    /// Snap the pen to nearby vertices, axes and parallel/perpendicular edges.
    pub smart_guides: bool,
    /// True while the ruler tool is dragging its origin or direction.
    pub ruler_dragging: bool,
    pub node_selection: Vec<NodeRef>,
    pub node_hover: Option<NodeRef>,
    /// Edge selected in the Node tool; its profile override is edited under
    /// "Advanced properties".
    pub edge_selection: Option<EdgeRef>,
    /// Whether the 2D canvas had keyboard focus last frame. Lets arrow nudges
    /// keep working when the canvas itself owns focus, without stealing arrows
    /// from focused text fields or sliders.
    pub canvas_has_focus: bool,
    pub svg_import_request: Option<PathBuf>,
    pub photo_import_request: Option<PathBuf>,
    pub alpha_import_request: Option<PathBuf>,
    pub normal_import_request: Option<PathBuf>,
    pub import_request: Option<PathBuf>,
    pub import_status: String,
    pub imported_mesh: Option<String>,
    /// True when the current imported mesh uses a bottom-left UV origin
    /// (OBJ/FBX/COLLADA/USD). glTF is top-left. This affects image row
    /// placement for bakes/overlays and the preview texture V flip; it does
    /// not change the tangent basis.
    pub uv_bottom_left: bool,
    /// Draw the UV island layout as an inset in the 3D preview.
    pub show_uv_overlay: bool,
    /// Draw the UV island layout over the 2D canvas as authoring guidance.
    pub show_uv_overlay_2d: bool,
    /// Paint a high-contrast reticle at the canvas pointer. Screen-sharing
    /// tools sometimes omit the OS cursor; this keeps it visible in the frame.
    pub show_canvas_cursor: bool,
    /// Opacity of both UV overlays, in 0..=1.
    pub uv_overlay_opacity: f32,
    /// Mirrors `MeshImport::udim.enabled` so layer creation and export can
    /// tag and split per-tile maps without borrowing the mesh resource.
    pub udim_enabled: bool,
    /// Tiles of the imported UDIM mesh, mirrored from `MeshImport`.
    pub udim_tiles: Vec<u32>,
    pub uv_tex_id: Option<egui::TextureId>,
    /// Image handle backing `uv_tex_id`, so reimports can release the old texture.
    pub uv_tex_handle: Option<Handle<Image>>,
    /// Borders-only layout for the 3D viewport inset.
    pub uv_inset_tex_id: Option<egui::TextureId>,
    pub uv_inset_tex_handle: Option<Handle<Image>>,
    pub uv_overlay_triangles: usize,
    pub canvas_pan_tool: bool,
    pub brush_radius: f32,
    pub brush_hardness: f32,
    pub brush_flow: f32,
    /// Maximum change a single mask stroke can make (flow builds up to it).
    pub brush_opacity: f32,
    /// Imported alpha and normal-map brush tips, plus the selected tip.
    pub brush_library: crate::brushes::BrushLibrary,
    /// Rotation applied to image brush tips, in degrees.
    pub brush_angle_deg: f32,
    /// Mirror or radial multiplication for every dab.
    pub symmetry: Symmetry,
    /// Number of copies made by radial symmetry.
    pub symmetry_radial_count: u32,
    /// Symmetry pivot in normalized document space (0..1).
    pub symmetry_pivot: Vec2,
    /// Latest pen/tablet pressure in 0..=1. `1.0` means no pressure data.
    pub pen_pressure: f32,
    /// True while a pressure-capable pen is in contact with the tablet.
    pub pen_pressure_active: bool,
    /// True once a device has reported force, so the UI can advertise pen support.
    pub pen_detected: bool,
    /// Time of the most recent pressure sample; used to drop stale pen state.
    pub pen_pressure_time: f64,
    /// Master switch for pressure-sensitive brush controls.
    pub pen_pressure_enabled: bool,
    /// Scale brush size with pen pressure.
    pub pen_pressure_size: bool,
    /// Scale brush flow with pen pressure.
    pub pen_pressure_flow: bool,
    /// Scale brush opacity with pen pressure.
    pub pen_pressure_opacity: bool,
    /// Two-finger touchpad scrolling pans the 2D canvas instead of zooming.
    pub touchpad_pan: bool,
    /// Whether the most recent scroll event came from a precision touchpad.
    pub last_scroll_touchpad: bool,
    pub brush_mode: PaintMode,
    /// Mask stamp colour: `true` adds white coverage, `false` cuts black.
    /// Kept separately from `brush_mode` so Smooth/Flatten brushes still stamp
    /// with the last chosen colour.
    pub mask_white: bool,
    pub brush_flatten_target: f32,
    pub stroke_width: f32,
    /// Width of the straight line cut drawn by the Line Lasso, in document px.
    pub lasso_line_width: f32,
    /// Snapshot undo/redo. Snapshots include the document, selection and
    /// collapsed groups so navigation context follows the edit.
    pub undo: Vec<UndoSnapshot>,
    pub redo: Vec<UndoSnapshot>,
    pub undo_gesture: Option<UndoSnapshot>,
    pub undo_gesture_committed: bool,
    /// Incremented only by document edits, not by evaluation-only changes.
    pub edit_serial: u64,
    /// Bumped by edits and evaluation toggles. Background results carrying an
    /// older serial are discarded instead of being uploaded.
    pub eval_serial: u64,
    /// Evaluate on the next frame instead of waiting for the throttle timer.
    pub force_eval: bool,
    pub material_preset: MaterialPreset,
    pub lighting_preset: LightingPreset,
    pub environment_preset: EnvironmentPreset,
    pub ui_initialized: bool,
    pub show_settings: bool,
    /// Blender bridge window. Opened from the status bar chip or Settings, so
    /// the switch and the connection steps are never more than one click away.
    pub show_bridge: bool,
    /// Floating duplicate-array dialog and its last settings.
    pub show_duplicate_array: bool,
    pub duplicate_array_count: u32,
    pub duplicate_array_offset: Vec2,
    /// Per-layer operation stacks, keyed by layer id. Entries are UI-only and
    /// are not serialized; an empty stack means the layer has no operations.
    pub operation_stacks: HashMap<LayerId, Vec<crate::operations::OperationEntry>>,
    /// Transient live preview used only by the evaluator. The project, undo
    /// history, layers dock and exports never see this document.
    pub operation_preview: Option<SharedDocument>,
    pub operation_preview_key: Option<crate::operations::OperationPreviewKey>,
    pub show_pbr: bool,
    pub viewport_tab: ViewportTab,
    /// Tab drawn on the previous frame, so a switch can start a transition
    /// without every call site having to announce itself.
    pub last_viewport_tab: ViewportTab,
    /// Cross-fade between the 3D preview and the 2D canvas, if one is running.
    pub view_transition: Option<ViewTransition>,
    /// Show the 3D preview and the 2D canvas at the same time.
    pub split_view: bool,
    /// Fraction of the split viewport occupied by the 3D pane.
    pub split_ratio: f32,
    /// Stack the split panes (3D above, 2D below) instead of side by side.
    pub split_stacked: bool,
    pub right_tab: RightTab,
    pub show_layers_dock: bool,
    pub show_right_dock: bool,
    /// Main document export selection and resolution. `0` means the document
    /// size; otherwise 512, 1024, 2048 or 4096.
    pub export_normal: bool,
    pub export_height: bool,
    pub export_project: bool,
    /// Derived maps generated from the final normal field.
    pub export_edge_wear: bool,
    pub export_curvature: bool,
    pub export_edge_detect: bool,
    /// Material-ID map generated from the layers' ID colours.
    pub export_id_map: bool,
    /// Edge-wear and edge-detect tuning, shown in the collapsible export rows.
    pub edge_wear_params: ns_core::derived::EdgeWearParams,
    pub edge_detect_params: ns_core::derived::EdgeDetectParams,
    /// Whether the derived-map parameter rows are expanded.
    pub export_edge_wear_open: bool,
    pub export_edge_detect_open: bool,
    /// How the exported height map is produced. Defaults to the legacy
    /// integration so old behavior stays reachable.
    pub export_height_source: HeightSource,
    pub export_resolution: u32,
    pub export_sixteen_bit: bool,
    /// Write one image per detected UDIM tile (`normal_1001.png`, …) instead
    /// of only the tile shown on the canvas.
    pub export_all_tiles: bool,
    /// Output folder for the main document export. `None` means `./output`.
    pub export_dir: Option<PathBuf>,
}

impl Default for EditorState {
    fn default() -> Self {
        Self {
            selected: None,
            multi_selected: Vec::new(),
            layer_clipboard: Vec::new(),
            collapsed: HashSet::new(),
            show_height: false,
            dirty: false,
            eval_timer: Timer::from_seconds(0.04, TimerMode::Repeating),
            zoom: 1.0,
            pan: egui::Vec2::ZERO,
            preview_rect: None,
            preview_shape: PreviewShape::Cube,
            preview_parallax: PreviewParallax::Relief,
            // Subtle by default: the height map adds depth without moving the
            // surface far enough to surprise anyone. Raise it in Settings when
            // fine-tuning the preview.
            preview_height_scale: 0.2,
            preview_tessellation: PreviewTessellation::Medium,
            auto_rotate: false,
            orbit_yaw: 0.35,
            orbit_pitch: 0.15,
            orbit_distance: 3.0,
            tiling: 1.0,
            tex_dims: [0, 0],
            normal_tex_id: None,
            height_tex_id: None,
            using_gpu: false,
            gate_rejects: Vec::new(),
            show_perf_hud: false,
            perf_scene_build_ms: PerfStat::default(),
            perf_cpu_eval_ms: PerfStat::default(),
            integrate_height: false,
            show_icon_sheet: false,
            preview_gizmo: PreviewGizmo::Off,
            show_light_gizmos: true,
            sweep_path_edit: false,
            sweep_path_drag: None,
            sweep_path_hover: None,
            sweep_path_selected: None,
            sweep_path_readout: String::new(),
            show_mask_overlay: false,
            mask_overlay_opacity: 0.55,
            mask_overlay_key: None,
            mask_overlay_grid: None,
            mask_overlay_tex_key: None,
            mask_overlay_tex: None,
            mask_edit_layer: None,
            mask_tex: None,
            mask_tex_key: None,
            mask_view: MaskView::default(),
            mask_stroke: None,
            curve_editor: None,
            last_eval_ms: 0.0,
            status: "Ready".into(),
            project_path: None,
            project_dir: None,
            project_saved_serial: 0,
            project_open_request: None,
            pending_lightmap: None,
            pending_lightmap_environment: None,
            pending_lightmap_preview: None,
            pending_bake: None,
            pending_bake_high: None,
            autosave_enabled: true,
            autosave_backups: crate::autosave::DEFAULT_BACKUPS,
            suppress_autosave: false,
            autosave_status: String::new(),
            autosave_error: None,
            canvas_tool: CanvasTool::Select,
            focus_text_content: false,
            text_edit_baseline: false,
            rect_corner: DEFAULT_RECT_CORNER,
            cable_width: 26.0,
            cable_height: 12.0,
            cable_cap: CableCap::Round,
            cable_cap_scale: 1.5,
            vector_tool: CanvasTool::Pen,
            shape_tool: CanvasTool::Rectangle,
            canvas_drag: None,
            snap_session: None,
            active_guides: Vec::new(),
            vector_draft: None,
            nudge_burst: false,
            lasso_tool: CanvasTool::LassoFree,
            lasso_mode: LassoMode::default(),
            lasso_draft: None,
            lasso_divide: false,
            scifi: crate::scifi::ScifiPanel::default(),
            ruler: None,
            ruler_locks_duplicates: true,
            smart_guides: true,
            ruler_dragging: false,
            node_selection: Vec::new(),
            node_hover: None,
            edge_selection: None,
            canvas_has_focus: false,
            svg_import_request: None,
            photo_import_request: None,
            alpha_import_request: None,
            normal_import_request: None,
            import_request: None,
            import_status: String::new(),
            imported_mesh: None,
            uv_bottom_left: false,
            show_uv_overlay: false,
            show_uv_overlay_2d: false,
            show_canvas_cursor: true,
            uv_overlay_opacity: 0.85,
            udim_enabled: false,
            udim_tiles: Vec::new(),
            uv_tex_id: None,
            uv_tex_handle: None,
            uv_inset_tex_id: None,
            uv_inset_tex_handle: None,
            uv_overlay_triangles: 0,
            canvas_pan_tool: false,
            brush_radius: 48.0,
            brush_hardness: 0.55,
            brush_flow: 0.35,
            brush_opacity: 1.0,
            brush_library: crate::brushes::BrushLibrary::default(),
            brush_angle_deg: 0.0,
            symmetry: Symmetry::Off,
            symmetry_radial_count: 6,
            symmetry_pivot: Vec2::splat(0.5),
            pen_pressure: 1.0,
            pen_pressure_active: false,
            pen_detected: false,
            pen_pressure_time: 0.0,
            pen_pressure_enabled: true,
            pen_pressure_size: true,
            pen_pressure_flow: true,
            pen_pressure_opacity: false,
            touchpad_pan: true,
            last_scroll_touchpad: true,
            brush_mode: PaintMode::Raise,
            mask_white: false,
            brush_flatten_target: 0.5,
            stroke_width: 8.0,
            lasso_line_width: 4.0,
            undo: Vec::new(),
            redo: Vec::new(),
            undo_gesture: None,
            undo_gesture_committed: false,
            edit_serial: 0,
            eval_serial: 0,
            force_eval: false,
            material_preset: MaterialPreset::default(),
            lighting_preset: LightingPreset::default(),
            environment_preset: EnvironmentPreset::default(),
            ui_initialized: false,
            show_settings: false,
            show_bridge: false,
            show_duplicate_array: false,
            duplicate_array_count: 3,
            duplicate_array_offset: Vec2::new(32.0, 0.0),
            operation_stacks: HashMap::new(),
            operation_preview: None,
            operation_preview_key: None,
            show_pbr: false,
            viewport_tab: ViewportTab::ThreeD,
            last_viewport_tab: ViewportTab::ThreeD,
            view_transition: None,
            split_view: false,
            split_ratio: 0.5,
            split_stacked: false,
            right_tab: RightTab::Properties,
            show_layers_dock: true,
            show_right_dock: true,
            export_normal: true,
            export_height: true,
            export_project: true,
            export_edge_wear: false,
            export_curvature: false,
            export_edge_detect: false,
            export_id_map: false,
            edge_wear_params: ns_core::derived::EdgeWearParams::default(),
            edge_detect_params: ns_core::derived::EdgeDetectParams::default(),
            export_edge_wear_open: false,
            export_edge_detect_open: false,
            export_height_source: HeightSource::default(),
            export_resolution: 0,
            export_sixteen_bit: true,
            export_all_tiles: true,
            export_dir: None,
        }
    }
}

impl EditorState {
    /// Drop the active drag's smart-guide targets and overlay. Transient state
    /// only: the document and undo history are untouched.
    pub fn clear_snap_guides(&mut self) {
        self.snap_session = None;
        self.active_guides.clear();
    }

    /// Mark the document as edited. This also advances the undo edit serial.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
        self.edit_serial = self.edit_serial.wrapping_add(1);
        self.eval_serial = self.eval_serial.wrapping_add(1);
    }

    /// Switch the UDIM tile the canvas evaluates. Layers tagged for the old
    /// tile stop contributing, so a selection that no longer applies is
    /// dropped instead of silently painting a hidden layer.
    pub fn set_active_tile(&mut self, doc: &mut DocumentRes, tile: u32) {
        if doc.0.active_tile == tile {
            return;
        }
        doc.0.active_tile = tile;
        if let Some(preview) = self.operation_preview.as_mut() {
            preview.active_tile = tile;
        }
        let applies = |path: &[usize]| {
            doc.0
                .layer_by_path(path)
                .is_some_and(|layer| layer.applies_to_tile(tile))
        };
        if self.selected.as_deref().is_some_and(|path| !applies(path)) {
            self.selected = None;
        }
        self.multi_selected.retain(|path| applies(path));
        self.request_eval_now();
        self.status = format!("UDIM tile {tile}");
    }

    /// Pressure the brush should use this frame. A mouse, a disabled pen or a
    /// lifted stylus all report full pressure so brush size stays predictable.
    pub fn brush_pressure(&self) -> f32 {
        if self.pen_pressure_enabled && self.pen_detected && self.pen_pressure_active {
            self.pen_pressure.clamp(0.0, 1.0)
        } else {
            1.0
        }
    }

    /// Brush radius after the pen pressure curve. Light strokes stay usable but
    /// visibly thinner; the minimum keeps a 0-pressure touch from vanishing.
    pub fn brush_radius_now(&self) -> f32 {
        let pressure = if self.pen_pressure_size {
            self.brush_pressure()
        } else {
            1.0
        };
        self.brush_radius * (0.2 + 0.8 * pressure)
    }

    /// Brush flow after the pen pressure curve. Flow is what makes light pen
    /// strokes build up slowly without changing the dab footprint.
    pub fn brush_flow_now(&self) -> f32 {
        let pressure = if self.pen_pressure_flow {
            self.brush_pressure()
        } else {
            1.0
        };
        self.brush_flow * (0.15 + 0.85 * pressure)
    }

    /// Per-dab opacity after the pen pressure curve. The mask stroke keeps this
    /// as its stroke cap; height and normal dabs multiply their alpha by it.
    pub fn brush_opacity_now(&self) -> f32 {
        let pressure = if self.pen_pressure_opacity {
            self.brush_pressure()
        } else {
            1.0
        };
        (self.brush_opacity * (0.15 + 0.85 * pressure)).clamp(0.0, 1.0)
    }

    /// True when the document changed since the last save or open. Evaluation
    /// and viewport changes do not count, so the title marker only tracks edits
    /// that belong in the project file.
    pub fn has_unsaved_changes(&self) -> bool {
        self.edit_serial != self.project_saved_serial
    }

    /// File name shown in the window title, without the directory.
    pub fn project_name(&self) -> Option<String> {
        self.project_path
            .as_ref()
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str())
            .map(str::to_owned)
    }

    /// Request evaluation without creating an undo entry (camera/lighting or
    /// evaluator toggles).
    pub fn mark_eval_dirty(&mut self) {
        self.dirty = true;
        self.eval_serial = self.eval_serial.wrapping_add(1);
    }

    /// Request an evaluation immediately (used when the visible map changes,
    /// such as leaving mask editing or switching to the height view).
    pub fn request_eval_now(&mut self) {
        self.dirty = true;
        self.eval_serial = self.eval_serial.wrapping_add(1);
        self.force_eval = true;
    }

    fn snapshot(&self, doc: &SharedDocument) -> UndoSnapshot {
        UndoSnapshot {
            doc: doc.clone(),
            selected: self.selected.clone(),
            multi_selected: self.multi_selected.clone(),
            collapsed: self.collapsed.clone(),
        }
    }

    pub fn begin_undo_gesture(&mut self, doc: &SharedDocument) {
        self.undo_gesture = Some(self.snapshot(doc));
        self.undo_gesture_committed = false;
    }

    /// Start a gesture only if this frame is not already inside one. Call this
    /// immediately before a document mutation so UI presses that never edit
    /// anything do not snapshot (and therefore do not clone) the document.
    pub fn ensure_undo_gesture(&mut self, doc: &SharedDocument) {
        if self.undo_gesture.is_none() && !self.undo_gesture_committed {
            self.begin_undo_gesture(doc);
        }
    }

    pub fn commit_undo_gesture(&mut self) {
        if self.undo_gesture_committed {
            return;
        }
        if let Some(snapshot) = self.undo_gesture.take() {
            self.push_undo_snapshot(snapshot);
            self.undo_gesture_committed = true;
        }
    }

    pub fn finish_undo_gesture(&mut self) {
        self.undo_gesture = None;
        self.undo_gesture_committed = false;
    }

    /// Explicit checkpoint for edits that do not start from a pointer gesture,
    /// such as imports completed on a later frame.
    pub fn checkpoint_document(&mut self, doc: &SharedDocument) {
        let _span = bevy::log::trace_span!("undo.checkpoint").entered();
        let snapshot = self.snapshot(doc);
        self.push_undo_snapshot(snapshot);
        self.finish_undo_gesture();
    }

    fn push_undo_snapshot(&mut self, snapshot: UndoSnapshot) {
        self.undo.push(snapshot);
        trim_snapshots(&mut self.undo, UNDO_MEMORY_BUDGET);
        self.redo.clear();
    }

    fn restore_snapshot(&mut self, snapshot: UndoSnapshot, doc: &mut DocumentRes) {
        doc.0 = snapshot.doc;
        self.selected = snapshot
            .selected
            .filter(|path| doc.0.layer_by_path(path).is_some());
        self.multi_selected = snapshot
            .multi_selected
            .into_iter()
            .filter(|path| doc.0.layer_by_path(path).is_some())
            .collect();
        self.collapsed = snapshot.collapsed;
        self.node_selection.clear();
        self.node_hover = None;
        self.edge_selection = None;
        self.canvas_drag = None;
        self.vector_draft = None;
        self.sweep_path_drag = None;
        self.sweep_path_hover = None;
        self.sweep_path_selected = None;
        self.sweep_path_readout.clear();
        self.mask_tex_key = None;
        self.finish_undo_gesture();
        self.mark_dirty();
        self.force_eval = true;
    }

    pub fn undo(&mut self, doc: &mut DocumentRes) -> bool {
        let Some(snapshot) = self.undo.pop() else {
            self.status = "Nothing to undo".into();
            return false;
        };
        let current = self.snapshot(&doc.0);
        self.redo.push(current);
        trim_snapshots(&mut self.redo, UNDO_MEMORY_BUDGET);
        self.restore_snapshot(snapshot, doc);
        self.status = "Undo".into();
        true
    }

    pub fn redo(&mut self, doc: &mut DocumentRes) -> bool {
        let Some(snapshot) = self.redo.pop() else {
            self.status = "Nothing to redo".into();
            return false;
        };
        let current = self.snapshot(&doc.0);
        self.undo.push(current);
        trim_snapshots(&mut self.undo, UNDO_MEMORY_BUDGET);
        self.restore_snapshot(snapshot, doc);
        self.status = "Redo".into();
        true
    }

    /// Bytes held by undo and redo, deduplicating shared document handles
    /// across both stacks.
    pub fn undo_bytes(&self) -> usize {
        let mut seen = HashSet::new();
        let mut bytes = 0usize;
        for snapshot in self.undo.iter().chain(self.redo.iter()) {
            bytes = bytes.saturating_add(snapshot.meta_bytes());
            if seen.insert(snapshot.doc.ptr()) {
                bytes = bytes.saturating_add(snapshot.doc.estimated_bytes());
            }
        }
        bytes
    }

    pub fn undo_budget(&self) -> usize {
        UNDO_MEMORY_BUDGET
    }
}

struct CpuEvalResult {
    eval_serial: u64,
    tex_dims: [usize; 2],
    normal_bytes: Vec<u8>,
    height_bytes: Option<Vec<u8>>,
    cache: EvalCache,
    elapsed_ms: f32,
}

struct CpuEvalJob {
    task: Task<Option<CpuEvalResult>>,
    token: ns_core::CancelToken,
    eval_serial: u64,
}

#[derive(Resource, Default)]
struct EvalRuntime {
    job: Option<CpuEvalJob>,
}

/// Evaluate the stack when the debounce timer fires. Shape-only stacks go
/// through the GPU compute node. Stacks that need the CPU evaluator run on the
/// compute pool so painting and property edits never block the UI thread.
#[allow(clippy::too_many_arguments)]
fn evaluate_when_dirty(
    time: Res<Time>,
    mut state: ResMut<EditorState>,
    doc: Res<DocumentRes>,
    mut cache: ResMut<EvalCacheRes>,
    mut eval: ResMut<EvalRuntime>,
    mut gpu_scene: ResMut<gpu::GpuScene>,
    mut gpu_cache: ResMut<gpu::GpuSceneCache>,
    handles: Res<PreviewHandles>,
    mut images: ResMut<Assets<Image>>,
) {
    state.eval_timer.tick(time.delta());

    // A newer edit supersedes the in-flight CPU job. Trip its token before
    // polling so it stops at the next layer boundary instead of running the
    // whole stack only to have the result discarded.
    if let Some(job) = &eval.job {
        if job.eval_serial != state.eval_serial {
            job.token.cancel();
        }
    }

    // Collect a finished background evaluation, if any.
    let mut finished_job = false;
    if let Some(mut job) = eval.job.take() {
        if let Some(result) = block_on(poll_once(&mut job.task)) {
            finished_job = true;
            if let Some(result) = result {
                cache.0 = result.cache;
                state.last_eval_ms = result.elapsed_ms;
                if result.eval_serial == state.eval_serial {
                    state.perf_cpu_eval_ms.push(result.elapsed_ms);
                    state.using_gpu = false;
                    state.tex_dims = result.tex_dims;
                    if let Some(mut image) = images.get_mut(&handles.normal_texture) {
                        image.data = Some(result.normal_bytes);
                    }
                    if let Some(bytes) = result.height_bytes {
                        if let Some(mut image) = images.get_mut(&handles.height_texture) {
                            image.data = Some(bytes);
                        }
                    }
                    state.dirty = false;
                }
            }
        } else {
            eval.job = Some(job);
        }
    }

    // In split view the 3D pane stays visible while painting, so keep the
    // composited texture current. In a sole 2D view the grayscale mask/overlay
    // covers the normal map, so deferring evaluation keeps strokes responsive.
    if state.mask_edit_layer.is_some()
        && !state.split_view
        && state.viewport_tab == ViewportTab::TwoD
        && !state.mask_view.shows_result()
    {
        return;
    }

    let ready = state.eval_timer.just_finished() || state.force_eval || finished_job;
    if !state.dirty || !ready {
        return;
    }
    let _span = bevy::log::trace_span!("evaluate").entered();

    // Operations preview through a temporary document: the evaluator, the 2D
    // canvas and the 3D material all read it, while the project stays untouched.
    let eval_doc = state
        .operation_preview
        .clone()
        .unwrap_or_else(|| doc.0.clone());
    let options = gpu::GpuSceneOptions {
        integrate_height: state.integrate_height,
    };
    let build_started = Instant::now();
    let gpu_result = {
        let _span = bevy::log::trace_span!("gpu.scene_build").entered();
        gpu::build_scene_reported(&eval_doc, options, &mut gpu_cache)
    };
    state
        .perf_scene_build_ms
        .push(build_started.elapsed().as_secs_f32() * 1000.0);
    match gpu_result {
        Ok(data) => {
            // A background CPU result is no longer needed. Cancelling the token
            // lets a worker that is still mid-stack stop early.
            if let Some(job) = eval.job.take() {
                job.token.cancel();
            }
            gpu_scene.data = Some(data);
            gpu_scene.generation = gpu_scene.generation.wrapping_add(1);
            state.using_gpu = true;
            state.gate_rejects.clear();
            state.dirty = false;
            state.force_eval = false;
            state.last_eval_ms = 0.0;
        }
        Err(rejects) => {
            state.gate_rejects = rejects;
            if eval.job.is_none() {
                gpu_scene.data = None;
                gpu_scene.generation = gpu_scene.generation.wrapping_add(1);
                state.using_gpu = false;
                state.dirty = false;
                state.force_eval = false;

                let shared = eval_doc;
                let mut task_cache = std::mem::take(&mut cache.0);
                let eval_serial = state.eval_serial;
                let integrate_height = state.integrate_height;
                // Height feeds the 2D height view and the 3D preview's parallax
                // depth map. Skipping it otherwise halves CPU evaluation time;
                // enabling either view requests a new evaluation immediately.
                let want_height =
                    state.show_height || state.preview_parallax != PreviewParallax::Off;
                let token = ns_core::CancelToken::new();
                let worker_token = token.clone();
                let task = AsyncComputeTaskPool::get().spawn(async move {
                    // Detach on the worker thread. Holding the shared handle for
                    // the whole evaluation would keep the live document shared,
                    // making the next mutable access from the Properties panel
                    // deep-clone it on the UI thread. The copy happens here
                    // instead, once, off-thread.
                    let snapshot: Document = (*shared).clone();
                    drop(shared);
                    let started = Instant::now();
                    let _span = bevy::log::trace_span!("eval.cpu").entered();
                    // Run the evaluator's nested rayon work in ns_core's own
                    // pool instead of the global pool Bevy also uses. The token
                    // is checked at layer boundaries and around the tree/height
                    // passes, so a superseded job stops early.
                    let evaluated: Option<(Grid<Vec3>, Option<Grid<f32>>)> = ns_core::eval_pool()
                        .install(|| {
                            let normals = snapshot.evaluate_auto_cancellable(
                                &mut task_cache,
                                ns_core::DEFAULT_EVAL_CACHE_BUDGET,
                                &worker_token,
                            )?;
                            let height = if want_height {
                                if integrate_height {
                                    if worker_token.is_cancelled() {
                                        return None;
                                    }
                                    let height = ns_core::normal_to_height(
                                        &normals,
                                        &ns_core::NormalToHeightOptions {
                                            wrap: snapshot.wrap,
                                            // Match the GPU integrator, which normalizes by min/max.
                                            percentile_low: 0.0,
                                            percentile_high: 1.0,
                                            ..Default::default()
                                        },
                                    );
                                    if worker_token.is_cancelled() {
                                        return None;
                                    }
                                    Some(height)
                                } else {
                                    Some(snapshot.evaluate_height_auto_cancellable(
                                        &mut task_cache,
                                        ns_core::DEFAULT_EVAL_CACHE_BUDGET,
                                        &worker_token,
                                    )?)
                                }
                            } else {
                                None
                            };
                            Some((normals, height))
                        });
                    let (normals, height) = evaluated?;
                    if worker_token.is_cancelled() {
                        return None;
                    }
                    let tex_dims = [normals.w as usize, normals.h as usize];
                    let normal_bytes = normal_rgba(&normals);
                    let height_bytes = height.as_ref().map(height_rgba);
                    Some(CpuEvalResult {
                        eval_serial,
                        tex_dims,
                        normal_bytes,
                        height_bytes,
                        cache: task_cache,
                        elapsed_ms: started.elapsed().as_secs_f32() * 1000.0,
                    })
                });
                eval.job = Some(CpuEvalJob {
                    task,
                    token,
                    eval_serial,
                });
            }
        }
    }

    let interval = if state.using_gpu { 0.025 } else { 0.10 };
    state
        .eval_timer
        .set_duration(Duration::from_secs_f32(interval));
}

pub fn normal_rgba(normals: &Grid<Vec3>) -> Vec<u8> {
    let w = normals.w as usize;
    let mut out = vec![0u8; w * normals.h as usize * 4];
    for y in 0..normals.h as usize {
        for x in 0..w {
            let n = encode_normal(*normals.get(x as u32, y as u32), GreenChannel::OpenGl);
            let i = (y * w + x) * 4;
            out[i] = to_u8(n.x);
            out[i + 1] = to_u8(n.y);
            out[i + 2] = to_u8(n.z);
            out[i + 3] = 255;
        }
    }
    out
}

pub fn height_rgba(height: &Grid<f32>) -> Vec<u8> {
    let w = height.w as usize;
    let mut out = vec![0u8; w * height.h as usize * 4];
    for y in 0..height.h as usize {
        for x in 0..w {
            let v = to_u8(*height.get(x as u32, y as u32));
            let i = (y * w + x) * 4;
            out[i] = v;
            out[i + 1] = v;
            out[i + 2] = v;
            out[i + 3] = 255;
        }
    }
    out
}

fn to_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::UVec2;
    use ns_core::Layer;

    #[test]
    fn perf_stat_reports_last_mean_and_p95() {
        let mut stat = PerfStat::default();
        assert_eq!(stat.last(), None);
        for value in [1.0, 2.0, 3.0, 4.0, 100.0] {
            stat.push(value);
        }
        assert_eq!(stat.last(), Some(100.0));
        assert!((stat.mean().unwrap() - 22.0).abs() < 1e-5);
        assert_eq!(stat.p95(), Some(100.0));
        stat.push(f32::NAN);
        assert_eq!(stat.len(), 5, "non-finite samples are ignored");
    }

    #[test]
    fn height_source_keeps_integrated_as_default() {
        assert_eq!(HeightSource::default(), HeightSource::Integrated);
        assert!(!HeightSource::Auto.integrates_height(false));
        assert!(HeightSource::Auto.integrates_height(true));
        assert!(!HeightSource::Direct.integrates_height(true));
        assert!(HeightSource::Integrated.integrates_height(false));
    }

    #[test]
    fn undo_and_redo_restore_document_and_selection() {
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(16)));
        doc.0.layers.push(Layer::flat("base"));
        let mut state = EditorState {
            selected: Some(vec![0]),
            ..Default::default()
        };
        state.checkpoint_document(&doc.0);

        doc.0.layers.push(Layer::flat("new"));
        state.mark_dirty();
        assert!(state.undo(&mut doc));
        assert_eq!(doc.0.layers.len(), 1);
        assert_eq!(doc.0.layers[0].name, "base");
        assert_eq!(state.selected, Some(vec![0]));

        assert!(state.redo(&mut doc));
        assert_eq!(doc.0.layers.len(), 2);
        assert_eq!(doc.0.layers[1].name, "new");
    }

    #[test]
    fn gesture_commit_records_one_undo_entry() {
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(16)));
        doc.0.layers.push(Layer::flat("base"));
        let mut state = EditorState::default();
        state.begin_undo_gesture(&doc.0);
        state.selected = Some(vec![0]);
        doc.0.layers[0].name = "edited".into();
        state.mark_dirty();
        state.commit_undo_gesture();
        state.finish_undo_gesture();
        assert_eq!(state.undo.len(), 1);
        assert!(state.undo(&mut doc));
        assert_eq!(doc.0.layers[0].name, "base");
    }

    #[test]
    fn undo_gesture_shares_the_document_until_the_first_edit() {
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(16)));
        doc.0.layers.push(Layer::flat("base"));
        let mut state = EditorState::default();

        // A selection-only click: the gesture is an O(1) handle clone.
        state.begin_undo_gesture(&doc.0);
        assert!(state
            .undo_gesture
            .as_ref()
            .is_some_and(|snapshot| snapshot.doc.shares_with(&doc.0)));
        state.finish_undo_gesture();

        // The first real edit detaches the live document from the snapshot.
        state.begin_undo_gesture(&doc.0);
        doc.0.layers.push(Layer::flat("new"));
        let snapshot = state.undo_gesture.as_ref().expect("gesture snapshot");
        assert!(!snapshot.doc.shares_with(&doc.0));
        assert_eq!(snapshot.doc.layers.len(), 1);
        assert_eq!(doc.0.layers.len(), 2);
    }

    #[test]
    #[ignore = "perf measurement; run with --ignored --nocapture"]
    fn perf_document_snapshot_clone() {
        use std::time::Instant;

        let mut doc = Document::new(UVec2::splat(1024));
        for i in 0..8 {
            let mut layer = Layer::shape(
                format!("shape {i}"),
                ShapeSource::Primitive(ns_core::Primitive::Rect {
                    center: Vec2::splat(512.0),
                    size: Vec2::splat(400.0),
                    corner_radius: 12.0,
                }),
            );
            if i < 4 {
                layer.mask = Some(Grid::new(1024, 1024, 1.0));
            }
            doc.layers.push(layer);
        }

        // Before: every undo checkpoint and CPU evaluation deep-cloned this.
        std::hint::black_box(doc.clone());
        let start = Instant::now();
        for _ in 0..5 {
            std::hint::black_box(doc.clone());
        }
        let deep_ms = start.elapsed().as_secs_f64() * 1000.0 / 5.0;

        // After: snapshots are Arc clones until the first mutation.
        let shared = SharedDocument::new(doc.clone());
        std::hint::black_box(shared.clone());
        let start = Instant::now();
        for _ in 0..100_000 {
            std::hint::black_box(shared.clone());
        }
        let shared_us = start.elapsed().as_secs_f64() * 1_000_000.0 / 100_000.0;

        println!(
            "document deep clone: {deep_ms:.2} ms | shared snapshot: {shared_us:.3} us | \
             ratio {:.0}x | document: {} MiB",
            deep_ms * 1000.0 / shared_us,
            doc.estimated_bytes() / (1024 * 1024)
        );
    }

    #[test]
    #[ignore = "perf measurement; run with --ignored --nocapture"]
    fn perf_drag_source_clone() {
        use std::time::Instant;

        let mut path = Path::new(ns_core::FillRule::NonZero);
        let mut subpath = ns_core::SubPath::new(true);
        for i in 0..5_000 {
            subpath
                .nodes
                .push(Node::corner(Vec2::new(i as f32, (i % 97) as f32)));
        }
        path.subpaths.push(subpath);
        let source = ShapeSource::Path(path);
        let id = Layer::flat("path").id;
        let drag = CanvasDrag::MoveGroup {
            start: Vec2::ZERO,
            sources: (0..8).map(|_| (id, source.clone())).collect(),
            axis: None,
        };

        // This is the clone `canvas_edit_drag_with_constraint` used to do on
        // every pointer move, before it started moving the drag out and back.
        std::hint::black_box(drag.clone());
        let start = Instant::now();
        for _ in 0..100 {
            std::hint::black_box(drag.clone());
        }
        let clone_ms = start.elapsed().as_secs_f64() * 1000.0 / 100.0;
        println!("old per-move CanvasDrag clone: {clone_ms:.3} ms (8 × 5,000-node paths)");
    }

    #[test]
    fn shared_document_evaluates_identically_to_its_snapshot() {
        let doc = DocumentRes::new(ns_core::demo_document(64));
        let snapshot = doc.0.clone();
        let mut live_cache = EvalCache::new();
        let mut snapshot_cache = EvalCache::new();
        let live = doc
            .0
            .evaluate_auto(&mut live_cache, ns_core::DEFAULT_EVAL_CACHE_BUDGET);
        let frozen =
            snapshot.evaluate_auto(&mut snapshot_cache, ns_core::DEFAULT_EVAL_CACHE_BUDGET);
        assert_eq!(live, frozen);
    }

    #[test]
    fn undo_keeps_the_newest_steps_within_a_memory_budget() {
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(64)));
        let mut state = EditorState::default();
        // Every snapshot carries exactly one 64x64 painted mask (16 KiB).
        for _ in 0..4 {
            doc.0.layers.clear();
            let mut layer = Layer::flat("masked");
            layer.mask = Some(Grid::new(64, 64, 1.0));
            doc.0.layers.push(layer);
            state.undo.push(state.snapshot(&doc.0));
        }
        // 40 KiB fits two snapshots; the newest are kept.
        trim_snapshots(&mut state.undo, 40 * 1024);
        assert_eq!(state.undo.len(), 2);
        assert_eq!(state.undo[1].doc.layers.len(), 1);
    }

    #[test]
    fn shared_snapshots_count_once_in_the_memory_budget() {
        let doc = DocumentRes::new(Document::new(UVec2::splat(32)));
        let state = EditorState::default();
        let one = vec![state.snapshot(&doc.0)];
        let two = vec![state.snapshot(&doc.0), state.snapshot(&doc.0)];
        assert!(one[0].doc.shares_with(&two[0].doc));
        assert_eq!(stack_bytes(&one), stack_bytes(&two));
    }

    #[test]
    fn a_single_oversized_snapshot_is_still_kept() {
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(64)));
        let mut layer = Layer::flat("big");
        layer.mask = Some(Grid::new(64, 64, 1.0));
        doc.0.layers.push(layer);
        let mut state = EditorState::default();
        state.undo.push(state.snapshot(&doc.0));
        // Even a budget smaller than one snapshot must leave the last step
        // undoable.
        trim_snapshots(&mut state.undo, 1);
        assert_eq!(state.undo.len(), 1);
    }

    #[test]
    fn the_depth_limit_still_caps_the_stack() {
        let doc = DocumentRes::new(Document::new(UVec2::splat(8)));
        let mut state = EditorState::default();
        for _ in 0..(UNDO_LIMIT + 4) {
            state.undo.push(state.snapshot(&doc.0));
        }
        trim_snapshots(&mut state.undo, usize::MAX);
        assert_eq!(state.undo.len(), UNDO_LIMIT);
    }

    #[test]
    fn photo_presets_scale_their_blur_radii() {
        let (_, radii, weights) = PhotoPreset::Stone.params(2.0);
        assert_eq!(radii, [4.0, 16.0, 48.0]);
        assert!(weights.iter().all(|weight| *weight > 0.0));
    }
}
