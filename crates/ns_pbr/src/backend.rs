//! Pluggable estimator backend interface (classical shipping path + future ML).

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;

use glam::Vec3;
use ns_core::Grid;

use crate::pixels::PbrImage;
use crate::types::{MapKind, MapToggles};
use crate::{PbrError, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendCaps {
    pub maps: MapToggles,
    pub native_tile: u32,
    pub vram_mb: u32,
    pub learned: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct TileConfig {
    pub tile_size: u32,
    /// Overlap as a fraction of the tile size (0.12..0.25 recommended).
    pub overlap: f32,
}

impl Default for TileConfig {
    fn default() -> Self {
        Self {
            tile_size: 1024,
            overlap: 0.18,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct EstimatedMaps {
    pub delit: Option<PbrImage>,
    pub normal: Option<Grid<Vec3>>,
    pub roughness: Option<Grid<f32>>,
    pub metallic: Option<Grid<f32>>,
    pub extras: HashMap<MapKind, Grid<f32>>,
}

/// A model or process that can estimate appearance-oriented maps from a
/// single image. The classical pipeline does not need one; ML backends can be
/// dropped in without touching the scheduler or app UI.
pub trait EstimatorBackend: Send + Sync {
    fn id(&self) -> &'static str;
    fn model_hash(&self) -> String;
    fn capabilities(&self) -> BackendCaps;
    fn estimate(
        &self,
        input: &PbrImage,
        want: &MapToggles,
        tile: TileConfig,
        cancel: &AtomicBool,
        progress: &dyn Fn(f32),
    ) -> Result<EstimatedMaps>;
}

/// Marker backend used when no learned model is installed. It advertises no
/// learned capabilities; all shipping maps are produced by the classical
/// pipeline instead.
#[derive(Clone, Copy, Debug, Default)]
pub struct ClassicalBackend;

impl EstimatorBackend for ClassicalBackend {
    fn id(&self) -> &'static str {
        "classical"
    }

    fn model_hash(&self) -> String {
        String::new()
    }

    fn capabilities(&self) -> BackendCaps {
        BackendCaps {
            maps: MapToggles::default(),
            native_tile: 0,
            vram_mb: 0,
            learned: false,
        }
    }

    fn estimate(
        &self,
        _input: &PbrImage,
        _want: &MapToggles,
        _tile: TileConfig,
        cancel: &AtomicBool,
        _progress: &dyn Fn(f32),
    ) -> Result<EstimatedMaps> {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(PbrError::Cancelled);
        }
        Err(PbrError::Message(
            "the classical backend runs through the stage pipeline".into(),
        ))
    }
}
