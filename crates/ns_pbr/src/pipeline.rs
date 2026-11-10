//! Stage graph execution, content-addressed keys and cached outputs.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use glam::Vec3;
use ns_core::Grid;

use crate::appearance::{metallic, roughness};
use crate::derived::{
    ao_maps, cavity, curvature, displacement, edge_wear, CurvatureOutput, DisplacementOutput,
};
use crate::geometry::{geometry, GeometryOutput};
use crate::pixels::PbrImage;
use crate::prep::{delit, prepare, DelitOutput, PrepOutput};
use crate::types::{MapKind, MetallicMode, PbrSettings, Stage};
use crate::{PbrError, Result};

pub const PIPELINE_VERSION: u32 = 1;
pub const BACKEND_ID: &str = "classical";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct StageKey(pub [u8; 32]);

impl StageKey {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

#[derive(Clone)]
pub enum StageOutput {
    Prep(PrepOutput),
    Delit(DelitOutput),
    Geometry(GeometryOutput),
    Displacement(DisplacementOutput),
    Curvature(CurvatureOutput),
    Ao(Grid<f32>),
    Cavity(Grid<f32>),
    EdgeWear(Grid<f32>),
    Roughness(Grid<f32>),
    Metallic(Grid<f32>),
}

impl StageOutput {
    pub fn approx_bytes(&self) -> usize {
        match self {
            StageOutput::Prep(prep) => prep.image.len() * 12 + prep.luma.data.len() * 4,
            StageOutput::Delit(delit) => delit.image.len() * 12 + delit.luma.data.len() * 4,
            StageOutput::Geometry(geometry) => {
                geometry.height.data.len() * 4 + geometry.normal.data.len() * 12
            }
            StageOutput::Displacement(displacement) => displacement.map.data.len() * 4,
            StageOutput::Curvature(curvature) => {
                curvature.values.data.len() * 8 + curvature.map.data.len() * 4
            }
            StageOutput::Ao(grid)
            | StageOutput::Cavity(grid)
            | StageOutput::EdgeWear(grid)
            | StageOutput::Roughness(grid)
            | StageOutput::Metallic(grid) => grid.data.len() * 4,
        }
    }

    pub fn height(&self) -> Option<&Grid<f32>> {
        match self {
            StageOutput::Geometry(geometry) => Some(&geometry.height),
            _ => None,
        }
    }

    pub fn normal(&self) -> Option<&Grid<Vec3>> {
        match self {
            StageOutput::Geometry(geometry) => Some(&geometry.normal),
            _ => None,
        }
    }

    pub fn delit_image(&self) -> Option<&PbrImage> {
        match self {
            StageOutput::Delit(delit) => Some(&delit.image),
            _ => None,
        }
    }

    pub fn displacement(&self) -> Option<&DisplacementOutput> {
        match self {
            StageOutput::Displacement(displacement) => Some(displacement),
            _ => None,
        }
    }

    pub fn curvature(&self) -> Option<&CurvatureOutput> {
        match self {
            StageOutput::Curvature(curvature) => Some(curvature),
            _ => None,
        }
    }

    pub fn scalar(&self) -> Option<&Grid<f32>> {
        match self {
            StageOutput::Ao(grid)
            | StageOutput::Cavity(grid)
            | StageOutput::EdgeWear(grid)
            | StageOutput::Roughness(grid)
            | StageOutput::Metallic(grid) => Some(grid),
            _ => None,
        }
    }
}

#[derive(Clone)]
pub struct PipelineOutput {
    pub stages: HashMap<Stage, Arc<StageOutput>>,
    pub keys: HashMap<Stage, StageKey>,
    /// New cache entries produced by this run (cached stages are omitted).
    pub new_entries: Vec<(StageKey, Arc<StageOutput>)>,
    pub timings_ms: HashMap<Stage, f32>,
}

impl PipelineOutput {
    pub fn output(&self, stage: Stage) -> Option<&StageOutput> {
        self.stages.get(&stage).map(AsRef::as_ref)
    }

    pub fn map(&self, kind: MapKind) -> Option<&StageOutput> {
        self.output(kind.stage())
    }
}

/// Compute the content-addressed key for every stage in the current plan.
pub fn stage_keys(
    settings: &PbrSettings,
    input_hash: [u8; 32],
) -> (Vec<Stage>, HashMap<Stage, StageKey>) {
    let plan = settings.plan();
    let mut keys = HashMap::new();
    for stage in &plan {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"ns_pbr");
        hasher.update(&PIPELINE_VERSION.to_le_bytes());
        hasher.update(BACKEND_ID.as_bytes());
        hasher.update(stage.label().as_bytes());
        hasher.update(&input_hash);
        hasher.update(&[quality_id(settings.quality), settings.seamless as u8]);
        if let Ok(bytes) = stage_settings_json(*stage, settings) {
            hasher.update(&bytes);
        }
        for dependency in key_dependencies(*stage, settings, &keys) {
            if let Some(key) = keys.get(&dependency) {
                hasher.update(key.as_bytes());
            }
        }
        keys.insert(*stage, StageKey(*hasher.finalize().as_bytes()));
    }
    (plan, keys)
}

fn key_dependencies(
    stage: Stage,
    settings: &PbrSettings,
    keys: &HashMap<Stage, StageKey>,
) -> Vec<Stage> {
    let mut deps = stage.deps().to_vec();
    match stage {
        Stage::Roughness => {
            if settings.roughness.use_cavity && keys.contains_key(&Stage::Cavity) {
                deps.push(Stage::Cavity);
            }
            if settings.roughness.use_wear && keys.contains_key(&Stage::EdgeWear) {
                deps.push(Stage::EdgeWear);
            }
        }
        Stage::Metallic
            if settings.metallic.mode == MetallicMode::FromEdgeWear
                && keys.contains_key(&Stage::EdgeWear) =>
        {
            deps.push(Stage::EdgeWear);
        }
        _ => {}
    }
    deps
}

fn stage_settings_json(stage: Stage, settings: &PbrSettings) -> serde_json::Result<Vec<u8>> {
    match stage {
        Stage::Prep => serde_json::to_vec(&(settings.quality, settings.seamless, settings.denoise)),
        Stage::Delit => serde_json::to_vec(&settings.delit),
        Stage::Geometry => serde_json::to_vec(&(&settings.height, settings.normal.strength)),
        Stage::Displacement => serde_json::to_vec(&settings.displacement),
        Stage::Curvature => serde_json::to_vec(&settings.curvature),
        Stage::Ao => serde_json::to_vec(&(&settings.ao, settings.quality)),
        Stage::Cavity => serde_json::to_vec(&settings.cavity),
        Stage::EdgeWear => serde_json::to_vec(&(&settings.edge_wear, settings.quality)),
        Stage::Roughness => serde_json::to_vec(&settings.roughness),
        Stage::Metallic => serde_json::to_vec(&settings.metallic),
    }
}

fn quality_id(quality: crate::types::Quality) -> u8 {
    match quality {
        crate::types::Quality::Draft => 0,
        crate::types::Quality::Standard => 1,
        crate::types::Quality::High => 2,
    }
}

/// Execute a plan on the current thread. The caller normally runs this on
/// Bevy's async compute pool. Stage boundaries and AO rows honour `cancel`.
pub fn run_pipeline(
    source: Arc<PbrImage>,
    settings: &PbrSettings,
    plan: &[Stage],
    keys: &HashMap<Stage, StageKey>,
    cached: &HashMap<StageKey, Arc<StageOutput>>,
    cancel: &AtomicBool,
    progress: &dyn Fn(Stage, f32),
) -> Result<PipelineOutput> {
    let mut outputs: HashMap<Stage, Arc<StageOutput>> = HashMap::new();
    let mut new_entries = Vec::new();
    let mut timings_ms = HashMap::new();

    for stage in plan {
        if cancel.load(Ordering::Relaxed) {
            return Err(PbrError::Cancelled);
        }
        progress(*stage, 0.0);
        let key = *keys
            .get(stage)
            .ok_or_else(|| PbrError::Message(format!("missing key for {:?}", stage)))?;
        if let Some(output) = cached.get(&key) {
            outputs.insert(*stage, output.clone());
            progress(*stage, 1.0);
            continue;
        }

        let started = Instant::now();
        let output = match stage {
            Stage::Prep => StageOutput::Prep(prepare(&source, settings, cancel)?),
            Stage::Delit => {
                let prep = as_prep(&outputs)?;
                StageOutput::Delit(delit(prep, settings, cancel)?)
            }
            Stage::Geometry => {
                let delit = as_delit(&outputs)?;
                StageOutput::Geometry(geometry(delit, settings, cancel)?)
            }
            Stage::Displacement => {
                let geometry = as_geometry(&outputs)?;
                StageOutput::Displacement(displacement(&geometry.height, settings, cancel)?)
            }
            Stage::Curvature => {
                let geometry = as_geometry(&outputs)?;
                StageOutput::Curvature(curvature(&geometry.height, settings, cancel)?)
            }
            Stage::Ao => {
                let geometry = as_geometry(&outputs)?;
                StageOutput::Ao(ao_maps(&geometry.height, settings, cancel)?)
            }
            Stage::Cavity => {
                let curvature = as_curvature(&outputs)?;
                StageOutput::Cavity(cavity(&curvature.values, settings))
            }
            Stage::EdgeWear => {
                let curvature = as_curvature(&outputs)?;
                let ao = as_scalar(&outputs, Stage::Ao)?;
                let geometry = as_geometry(&outputs)?;
                StageOutput::EdgeWear(edge_wear(
                    curvature,
                    ao,
                    &geometry.height,
                    settings,
                    cancel,
                )?)
            }
            Stage::Roughness => {
                let delit = as_delit(&outputs)?;
                let geometry = as_geometry(&outputs)?;
                let cavity_map = optional_scalar(&outputs, Stage::Cavity);
                let wear_map = optional_scalar(&outputs, Stage::EdgeWear);
                StageOutput::Roughness(roughness(
                    delit, geometry, cavity_map, wear_map, settings, cancel,
                )?)
            }
            Stage::Metallic => {
                let delit = as_delit(&outputs)?;
                let geometry = as_geometry(&outputs)?;
                let wear_map = optional_scalar(&outputs, Stage::EdgeWear);
                StageOutput::Metallic(metallic(delit, geometry, wear_map, settings, cancel)?)
            }
        };
        let output = Arc::new(output);
        timings_ms.insert(*stage, started.elapsed().as_secs_f32() * 1000.0);
        outputs.insert(*stage, output.clone());
        new_entries.push((key, output));
        progress(*stage, 1.0);
    }

    Ok(PipelineOutput {
        stages: outputs,
        keys: keys.clone(),
        new_entries,
        timings_ms,
    })
}

fn as_prep(outputs: &HashMap<Stage, Arc<StageOutput>>) -> Result<&PrepOutput> {
    match outputs.get(&Stage::Prep).map(AsRef::as_ref) {
        Some(StageOutput::Prep(prep)) => Ok(prep),
        _ => Err(PbrError::Message("Prep dependency missing".into())),
    }
}

fn as_delit(outputs: &HashMap<Stage, Arc<StageOutput>>) -> Result<&DelitOutput> {
    match outputs.get(&Stage::Delit).map(AsRef::as_ref) {
        Some(StageOutput::Delit(delit)) => Ok(delit),
        _ => Err(PbrError::Message("Delit dependency missing".into())),
    }
}

fn as_geometry(outputs: &HashMap<Stage, Arc<StageOutput>>) -> Result<&GeometryOutput> {
    match outputs.get(&Stage::Geometry).map(AsRef::as_ref) {
        Some(StageOutput::Geometry(geometry)) => Ok(geometry),
        _ => Err(PbrError::Message("Geometry dependency missing".into())),
    }
}

fn as_curvature(outputs: &HashMap<Stage, Arc<StageOutput>>) -> Result<&CurvatureOutput> {
    match outputs.get(&Stage::Curvature).map(AsRef::as_ref) {
        Some(StageOutput::Curvature(curvature)) => Ok(curvature),
        _ => Err(PbrError::Message("Curvature dependency missing".into())),
    }
}

fn as_scalar(outputs: &HashMap<Stage, Arc<StageOutput>>, stage: Stage) -> Result<&Grid<f32>> {
    match outputs.get(&stage).map(AsRef::as_ref) {
        Some(output) => output
            .scalar()
            .ok_or_else(|| PbrError::Message(format!("{stage:?} is not a scalar output"))),
        None => Err(PbrError::Message(format!("{stage:?} dependency missing"))),
    }
}

fn optional_scalar(outputs: &HashMap<Stage, Arc<StageOutput>>, stage: Stage) -> Option<&Grid<f32>> {
    outputs.get(&stage).and_then(|output| output.scalar())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{MapToggles, Quality};
    use glam::Vec3;
    use std::sync::atomic::AtomicBool;

    fn toy_settings() -> PbrSettings {
        PbrSettings {
            quality: Quality::Draft,
            maps: MapToggles {
                normal: true,
                height: true,
                ao: true,
                cavity: true,
                roughness: true,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn keys_are_stable_and_stage_scoped() {
        let settings = toy_settings();
        let (_, a) = stage_keys(&settings, [7u8; 32]);
        let (_, b) = stage_keys(&settings, [7u8; 32]);
        assert_eq!(a, b);

        let mut changed = settings.clone();
        changed.ao.directions = 32;
        let (_, c) = stage_keys(&changed, [7u8; 32]);
        assert_eq!(a[&Stage::Geometry], c[&Stage::Geometry]);
        assert_ne!(a[&Stage::Ao], c[&Stage::Ao]);

        let mut changed = settings;
        changed.height.band_weights[0] += 0.1;
        let (_, d) = stage_keys(&changed, [7u8; 32]);
        assert_ne!(a[&Stage::Geometry], d[&Stage::Geometry]);
        assert_ne!(a[&Stage::Ao], d[&Stage::Ao]);
    }

    #[test]
    fn pipeline_produces_finite_bounded_stages() {
        let source = PbrImage::from_vec(
            32,
            32,
            (0..32)
                .flat_map(|y| {
                    (0..32).map(move |x| {
                        let value = ((x as f32 * 0.4).sin() * (y as f32 * 0.3).cos()).abs();
                        Vec3::new(value * 0.6 + 0.2, value * 0.5 + 0.25, value * 0.4 + 0.3)
                    })
                })
                .collect(),
        );
        let settings = toy_settings();
        let (plan, keys) = stage_keys(&settings, [3u8; 32]);
        let output = run_pipeline(
            Arc::new(source),
            &settings,
            &plan,
            &keys,
            &HashMap::new(),
            &AtomicBool::new(false),
            &|_, _| {},
        )
        .unwrap();
        for stage in plan {
            assert!(output.output(stage).is_some(), "{stage:?}");
        }
        let normal = output.output(Stage::Geometry).unwrap().normal().unwrap();
        for n in &normal.data {
            assert!((n.length() - 1.0).abs() < 1e-3);
        }
        for stage in [Stage::Ao, Stage::Cavity, Stage::Roughness] {
            for value in &output.output(stage).unwrap().scalar().unwrap().data {
                assert!((0.0..=1.0).contains(value), "{stage:?}: {value}");
            }
        }
    }

    #[test]
    fn cancellation_returns_cancelled() {
        let source = PbrImage::new(16, 16, Vec3::splat(0.5));
        let settings = toy_settings();
        let (plan, keys) = stage_keys(&settings, [1u8; 32]);
        let cancel = AtomicBool::new(true);
        let result = run_pipeline(
            Arc::new(source),
            &settings,
            &plan,
            &keys,
            &HashMap::new(),
            &cancel,
            &|_, _| {},
        );
        assert!(matches!(result, Err(PbrError::Cancelled)));
    }

    #[test]
    fn pipeline_is_deterministic() {
        let source = PbrImage::from_vec(
            24,
            24,
            (0..24)
                .flat_map(|y| {
                    (0..24).map(move |x| {
                        let value = ((x as f32 * 0.5).sin() * (y as f32 * 0.7).cos()).abs();
                        Vec3::new(value * 0.7 + 0.1, value * 0.5 + 0.2, value * 0.3 + 0.3)
                    })
                })
                .collect(),
        );
        let settings = toy_settings();
        let (plan, keys) = stage_keys(&settings, [5u8; 32]);
        let run = || {
            run_pipeline(
                Arc::new(source.clone()),
                &settings,
                &plan,
                &keys,
                &HashMap::new(),
                &AtomicBool::new(false),
                &|_, _| {},
            )
            .unwrap()
        };
        let first = run();
        let second = run();
        for stage in [Stage::Geometry, Stage::Ao, Stage::Cavity, Stage::Roughness] {
            let a = first.output(stage).unwrap();
            let b = second.output(stage).unwrap();
            match (a, b) {
                (StageOutput::Geometry(a), StageOutput::Geometry(b)) => {
                    assert_eq!(a.height.data, b.height.data);
                    assert_eq!(a.normal.data, b.normal.data);
                }
                (a, b) => {
                    assert_eq!(a.scalar().unwrap().data, b.scalar().unwrap().data);
                }
            }
        }
    }

    #[test]
    fn cached_stage_is_reused() {
        let source = PbrImage::new(16, 16, Vec3::splat(0.5));
        let settings = toy_settings();
        let (plan, keys) = stage_keys(&settings, [2u8; 32]);
        let first = run_pipeline(
            Arc::new(source.clone()),
            &settings,
            &plan,
            &keys,
            &HashMap::new(),
            &AtomicBool::new(false),
            &|_, _| {},
        )
        .unwrap();
        let cache: HashMap<_, _> = first.new_entries.iter().cloned().collect();
        let second = run_pipeline(
            Arc::new(source),
            &settings,
            &plan,
            &keys,
            &cache,
            &AtomicBool::new(false),
            &|_, _| {},
        )
        .unwrap();
        assert!(second.new_entries.is_empty());
    }
}
