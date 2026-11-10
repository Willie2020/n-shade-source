//! Map/stage graph and user-facing settings for single-image PBR generation.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum MapKind {
    Normal,
    Height,
    Displacement,
    Ao,
    Cavity,
    Curvature,
    EdgeWear,
    Roughness,
    Metallic,
    Delit,
}

impl MapKind {
    pub const ALL: [MapKind; 10] = [
        MapKind::Normal,
        MapKind::Height,
        MapKind::Displacement,
        MapKind::Ao,
        MapKind::Cavity,
        MapKind::Curvature,
        MapKind::EdgeWear,
        MapKind::Roughness,
        MapKind::Metallic,
        MapKind::Delit,
    ];

    pub fn stage(self) -> Stage {
        match self {
            MapKind::Normal | MapKind::Height => Stage::Geometry,
            MapKind::Displacement => Stage::Displacement,
            MapKind::Ao => Stage::Ao,
            MapKind::Cavity => Stage::Cavity,
            MapKind::Curvature => Stage::Curvature,
            MapKind::EdgeWear => Stage::EdgeWear,
            MapKind::Roughness => Stage::Roughness,
            MapKind::Metallic => Stage::Metallic,
            MapKind::Delit => Stage::Delit,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            MapKind::Normal => "Normal",
            MapKind::Height => "Height",
            MapKind::Displacement => "Displacement",
            MapKind::Ao => "Ambient Occlusion",
            MapKind::Cavity => "Cavity",
            MapKind::Curvature => "Curvature",
            MapKind::EdgeWear => "Edge Wear",
            MapKind::Roughness => "Roughness",
            MapKind::Metallic => "Metallic",
            MapKind::Delit => "Base Colour (Delit)",
        }
    }

    pub fn suffix(self) -> &'static str {
        match self {
            MapKind::Normal => "normal",
            MapKind::Height => "height",
            MapKind::Displacement => "displacement",
            MapKind::Ao => "ao",
            MapKind::Cavity => "cavity",
            MapKind::Curvature => "curvature",
            MapKind::EdgeWear => "edge_wear",
            MapKind::Roughness => "roughness",
            MapKind::Metallic => "metallic",
            MapKind::Delit => "base_color",
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum Stage {
    Prep,
    Delit,
    Geometry,
    Displacement,
    Curvature,
    Ao,
    Cavity,
    EdgeWear,
    Roughness,
    Metallic,
}

impl Stage {
    pub const ALL: [Stage; 10] = [
        Stage::Prep,
        Stage::Delit,
        Stage::Geometry,
        Stage::Displacement,
        Stage::Curvature,
        Stage::Ao,
        Stage::Cavity,
        Stage::EdgeWear,
        Stage::Roughness,
        Stage::Metallic,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Stage::Prep => "Prep",
            Stage::Delit => "Delit",
            Stage::Geometry => "Geometry",
            Stage::Displacement => "Displacement",
            Stage::Curvature => "Curvature",
            Stage::Ao => "AO",
            Stage::Cavity => "Cavity",
            Stage::EdgeWear => "Edge Wear",
            Stage::Roughness => "Roughness",
            Stage::Metallic => "Metallic",
        }
    }

    /// Static dependency graph from the specification.
    pub fn deps(self) -> &'static [Stage] {
        match self {
            Stage::Prep => &[],
            Stage::Delit => &[Stage::Prep],
            Stage::Geometry => &[Stage::Delit],
            Stage::Displacement | Stage::Curvature | Stage::Ao => &[Stage::Geometry],
            Stage::Cavity => &[Stage::Curvature],
            Stage::EdgeWear => &[Stage::Curvature, Stage::Ao],
            Stage::Roughness => &[Stage::Delit, Stage::Geometry],
            Stage::Metallic => &[Stage::Delit, Stage::Roughness],
        }
    }
}

/// Post-order dependency plan. Enabling a map also pulls in its dependencies.
pub fn plan(enabled: impl Iterator<Item = MapKind>) -> Vec<Stage> {
    fn visit(stage: Stage, seen: &mut HashSet<Stage>, out: &mut Vec<Stage>) {
        if !seen.insert(stage) {
            return;
        }
        for dependency in stage.deps() {
            visit(*dependency, seen, out);
        }
        out.push(stage);
    }

    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for kind in enabled {
        visit(kind.stage(), &mut seen, &mut out);
    }
    out
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum Quality {
    Draft,
    #[default]
    Standard,
    High,
}

impl Quality {
    pub const ALL: [Quality; 3] = [Quality::Draft, Quality::Standard, Quality::High];

    pub fn label(self) -> &'static str {
        match self {
            Quality::Draft => "Draft",
            Quality::Standard => "Standard",
            Quality::High => "High",
        }
    }

    /// Working resolution, never upscaling beyond the source.
    pub fn working_size(self, width: u32, height: u32) -> (u32, u32) {
        let target = match self {
            Quality::Draft => 512,
            Quality::Standard => 1024,
            Quality::High => u32::MAX,
        };
        let long = width.max(height);
        if long <= target {
            return (width, height);
        }
        let scale = target as f32 / long as f32;
        (
            (width as f32 * scale).round().max(1.0) as u32,
            (height as f32 * scale).round().max(1.0) as u32,
        )
    }

    pub fn ao_directions(self) -> Option<usize> {
        match self {
            Quality::Draft => Some(8),
            Quality::Standard => None,
            Quality::High => Some(32),
        }
    }

    pub fn ao_steps(self) -> usize {
        match self {
            Quality::Draft => 6,
            Quality::Standard => 8,
            Quality::High => 12,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct MapToggles {
    pub normal: bool,
    pub height: bool,
    pub displacement: bool,
    pub ao: bool,
    pub cavity: bool,
    pub curvature: bool,
    pub edge_wear: bool,
    pub roughness: bool,
    pub metallic: bool,
    pub delit: bool,
}

impl MapToggles {
    pub fn enabled(self, kind: MapKind) -> bool {
        match kind {
            MapKind::Normal => self.normal,
            MapKind::Height => self.height,
            MapKind::Displacement => self.displacement,
            MapKind::Ao => self.ao,
            MapKind::Cavity => self.cavity,
            MapKind::Curvature => self.curvature,
            MapKind::EdgeWear => self.edge_wear,
            MapKind::Roughness => self.roughness,
            MapKind::Metallic => self.metallic,
            MapKind::Delit => self.delit,
        }
    }

    pub fn set(&mut self, kind: MapKind, enabled: bool) {
        match kind {
            MapKind::Normal => self.normal = enabled,
            MapKind::Height => self.height = enabled,
            MapKind::Displacement => self.displacement = enabled,
            MapKind::Ao => self.ao = enabled,
            MapKind::Cavity => self.cavity = enabled,
            MapKind::Curvature => self.curvature = enabled,
            MapKind::EdgeWear => self.edge_wear = enabled,
            MapKind::Roughness => self.roughness = enabled,
            MapKind::Metallic => self.metallic = enabled,
            MapKind::Delit => self.delit = enabled,
        }
    }

    pub fn enabled_iter(self) -> impl Iterator<Item = MapKind> {
        MapKind::ALL
            .into_iter()
            .filter(move |kind| self.enabled(*kind))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PbrSettings {
    pub maps: MapToggles,
    pub quality: Quality,
    /// Periodic addressing for every kernel (wrap mode).
    pub seamless: bool,
    /// Light bilateral pre-filter before derivatives.
    pub denoise: bool,
    pub delit: DelitSettings,
    pub height: HeightSettings,
    pub normal: NormalSettings,
    pub displacement: DisplacementSettings,
    pub ao: AoSettings,
    pub curvature: CurvatureSettings,
    pub cavity: CavitySettings,
    pub edge_wear: EdgeWearSettings,
    pub roughness: RoughnessSettings,
    pub metallic: MetallicSettings,
}

impl PbrSettings {
    /// Plan for the current toggles, including optional dependencies selected
    /// by roughness/metallic modes.
    pub fn plan(&self) -> Vec<Stage> {
        let mut kinds: Vec<MapKind> = self.maps.enabled_iter().collect();
        if self.roughness.use_cavity && (self.maps.roughness || self.maps.metallic) {
            kinds.push(MapKind::Cavity);
        }
        if self.roughness.use_wear && (self.maps.roughness || self.maps.metallic) {
            kinds.push(MapKind::EdgeWear);
        }
        if self.metallic.mode == MetallicMode::FromEdgeWear
            && (self.maps.metallic || self.maps.edge_wear)
        {
            kinds.push(MapKind::EdgeWear);
        }
        kinds.sort_by_key(|kind| MapKind::ALL.iter().position(|k| k == kind));
        kinds.dedup();
        plan(kinds.into_iter())
    }

    pub fn enabled_maps(&self) -> impl Iterator<Item = MapKind> + '_ {
        self.maps.enabled_iter()
    }

    pub fn wrap(&self) -> ns_core::Wrap {
        if self.seamless {
            ns_core::Wrap::Repeat
        } else {
            ns_core::Wrap::Clamp
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DelitSettings {
    /// Gaussian sigma as a fraction of the short side.
    pub sigma_fraction: f32,
    /// Percentile used to soft-clamp baked highlights.
    pub highlight_percentile: f32,
}

impl Default for DelitSettings {
    fn default() -> Self {
        Self {
            sigma_fraction: 0.10,
            highlight_percentile: 0.995,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HeightSettings {
    pub invert: bool,
    /// Six band weights, fine to coarse.
    pub band_weights: [f32; 6],
    /// Large-scale drift removal, 0..1.
    pub drift_removal: f32,
    /// Height range in pixels used by normals, AO and displacement.
    pub height_scale_px: f32,
}

impl Default for HeightSettings {
    fn default() -> Self {
        Self {
            invert: false,
            band_weights: [0.10, 0.20, 0.30, 0.25, 0.10, 0.05],
            drift_removal: 0.0,
            height_scale_px: 8.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NormalSettings {
    pub strength: f32,
    /// When true the exported normal is DirectX green; the internal preview
    /// remains OpenGL.
    pub direct_x: bool,
}

impl Default for NormalSettings {
    fn default() -> Self {
        Self {
            strength: 1.0,
            direct_x: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DisplacementSettings {
    pub midlevel: f32,
    pub amount_m: f32,
    pub drift_removal: f32,
}

impl Default for DisplacementSettings {
    fn default() -> Self {
        Self {
            midlevel: 0.5,
            amount_m: 0.01,
            drift_removal: 0.8,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AoSettings {
    pub small_radius_px: f32,
    pub large_radius_px: f32,
    pub directions: usize,
    pub steps: usize,
    pub strength: f32,
    pub blur_range: f32,
}

impl Default for AoSettings {
    fn default() -> Self {
        Self {
            small_radius_px: 4.0,
            large_radius_px: 32.0,
            directions: 16,
            steps: 8,
            strength: 1.0,
            blur_range: 0.08,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CurvatureSettings {
    pub scales: [f32; 3],
    pub weights: [f32; 3],
    pub gain: f32,
}

impl Default for CurvatureSettings {
    fn default() -> Self {
        Self {
            scales: [1.0, 2.0, 4.0],
            weights: [0.5, 0.3, 0.2],
            gain: 0.5,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CavitySettings {
    pub lo: f32,
    pub hi: f32,
    pub invert: bool,
}

impl Default for CavitySettings {
    fn default() -> Self {
        Self {
            lo: 0.10,
            hi: 0.60,
            invert: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EdgeWearSettings {
    /// Curvature smoothing in pixels.
    pub width_px: f32,
    /// Convexity thresholds for the base edge mask.
    pub lo: f32,
    pub hi: f32,
    pub intensity: f32,
    pub breakup: f32,
    pub noise_frequency: u32,
    pub noise_octaves: u32,
    pub contrast: f32,
    pub ao_influence: f32,
    pub height_bias: f32,
    pub scratch_amount: f32,
    pub seed: u32,
}

impl Default for EdgeWearSettings {
    fn default() -> Self {
        Self {
            width_px: 2.0,
            lo: 0.15,
            hi: 0.60,
            intensity: 1.0,
            breakup: 0.7,
            noise_frequency: 6,
            noise_octaves: 4,
            contrast: 0.3,
            ao_influence: 0.5,
            height_bias: 0.0,
            scratch_amount: 0.0,
            seed: 1,
        }
    }
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum MaterialClass {
    #[default]
    RoughStone,
    Wood,
    FabricLeather,
    PlasticPainted,
    PolishedMetal,
    BrushedMetal,
    SoilSand,
}

impl MaterialClass {
    pub const ALL: [MaterialClass; 7] = [
        MaterialClass::RoughStone,
        MaterialClass::Wood,
        MaterialClass::FabricLeather,
        MaterialClass::PlasticPainted,
        MaterialClass::PolishedMetal,
        MaterialClass::BrushedMetal,
        MaterialClass::SoilSand,
    ];

    pub fn label(self) -> &'static str {
        match self {
            MaterialClass::RoughStone => "Rough stone / concrete",
            MaterialClass::Wood => "Wood",
            MaterialClass::FabricLeather => "Fabric / leather",
            MaterialClass::PlasticPainted => "Plastic / painted",
            MaterialClass::PolishedMetal => "Metal, polished",
            MaterialClass::BrushedMetal => "Metal, brushed / worn",
            MaterialClass::SoilSand => "Soil / mud / sand",
        }
    }

    /// (low, high, base, default `k_wear`)
    pub fn range(self) -> (f32, f32, f32, f32) {
        match self {
            MaterialClass::RoughStone => (0.55, 0.95, 0.75, 0.10),
            MaterialClass::Wood => (0.40, 0.85, 0.60, 0.05),
            MaterialClass::FabricLeather => (0.60, 0.95, 0.85, 0.05),
            MaterialClass::PlasticPainted => (0.25, 0.65, 0.40, 0.12),
            MaterialClass::PolishedMetal => (0.10, 0.45, 0.25, -0.15),
            MaterialClass::BrushedMetal => (0.25, 0.75, 0.50, -0.08),
            MaterialClass::SoilSand => (0.70, 1.00, 0.90, 0.05),
        }
    }
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum RoughnessMode {
    #[default]
    Heuristic,
    Constant,
}

impl RoughnessMode {
    pub const ALL: [RoughnessMode; 2] = [RoughnessMode::Heuristic, RoughnessMode::Constant];

    pub fn label(self) -> &'static str {
        match self {
            RoughnessMode::Heuristic => "Heuristic",
            RoughnessMode::Constant => "Constant",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoughnessSettings {
    pub class: MaterialClass,
    pub mode: RoughnessMode,
    pub constant: f32,
    pub k_hf: f32,
    pub k_cav: f32,
    pub k_wear: f32,
    pub use_cavity: bool,
    pub use_wear: bool,
}

impl Default for RoughnessSettings {
    fn default() -> Self {
        Self {
            class: MaterialClass::RoughStone,
            mode: RoughnessMode::Heuristic,
            constant: 0.6,
            k_hf: 0.3,
            k_cav: 0.2,
            k_wear: 0.10,
            use_cavity: true,
            use_wear: false,
        }
    }
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum MetallicMode {
    #[default]
    None,
    Constant,
    FromEdgeWear,
    AutoSegment,
}

impl MetallicMode {
    pub const ALL: [MetallicMode; 4] = [
        MetallicMode::None,
        MetallicMode::Constant,
        MetallicMode::FromEdgeWear,
        MetallicMode::AutoSegment,
    ];

    pub fn label(self) -> &'static str {
        match self {
            MetallicMode::None => "None (0)",
            MetallicMode::Constant => "Constant",
            MetallicMode::FromEdgeWear => "From edge wear",
            MetallicMode::AutoSegment => "Auto-segment",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MetallicSettings {
    pub mode: MetallicMode,
    pub constant: f32,
    pub threshold: f32,
}

impl Default for MetallicSettings {
    fn default() -> Self {
        Self {
            mode: MetallicMode::None,
            constant: 1.0,
            threshold: 0.5,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_is_topological_and_complete() {
        let stages = plan([MapKind::Normal, MapKind::EdgeWear].into_iter());
        let index = |stage: Stage| stages.iter().position(|s| *s == stage).unwrap();
        assert_eq!(stages.len(), 6);
        assert!(index(Stage::Prep) < index(Stage::Delit));
        assert!(index(Stage::Delit) < index(Stage::Geometry));
        assert!(index(Stage::Geometry) < index(Stage::Ao));
        assert!(index(Stage::Geometry) < index(Stage::Curvature));
        assert!(index(Stage::Curvature) < index(Stage::EdgeWear));
        assert!(index(Stage::Ao) < index(Stage::EdgeWear));
    }

    #[test]
    fn empty_plan_is_empty() {
        assert!(plan(std::iter::empty()).is_empty());
    }

    #[test]
    fn settings_pull_in_optional_dependencies() {
        let mut settings = PbrSettings {
            maps: MapToggles {
                roughness: true,
                ..Default::default()
            },
            ..Default::default()
        };
        settings.roughness.use_cavity = true;
        let stages = settings.plan();
        assert!(stages.contains(&Stage::Cavity));
        assert!(stages.contains(&Stage::Curvature));

        let mut settings = PbrSettings {
            maps: MapToggles {
                metallic: true,
                ..Default::default()
            },
            ..Default::default()
        };
        settings.metallic.mode = MetallicMode::FromEdgeWear;
        let stages = settings.plan();
        assert!(stages.contains(&Stage::EdgeWear));
        assert!(stages.contains(&Stage::Ao));
    }

    #[test]
    fn working_size_never_upscales() {
        assert_eq!(Quality::Draft.working_size(4096, 2048), (512, 256));
        assert_eq!(Quality::Standard.working_size(4096, 2048), (1024, 512));
        assert_eq!(Quality::High.working_size(4096, 2048), (4096, 2048));
        assert_eq!(Quality::High.working_size(320, 200), (320, 200));
    }

    #[test]
    fn toggles_iterate_in_display_order() {
        let toggles = MapToggles {
            normal: true,
            roughness: true,
            ..Default::default()
        };
        let enabled: Vec<_> = toggles.enabled_iter().collect();
        assert_eq!(enabled, vec![MapKind::Normal, MapKind::Roughness]);
    }
}
