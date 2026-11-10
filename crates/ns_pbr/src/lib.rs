//! Single-image PBR map generation for N-shade.
//!
//! The pipeline is engine-agnostic and classical-first: it works with no
//! model installed, keeps every intermediate in linear f32, and exposes a
//! stage graph so shared results are computed once. ML backends can be added
//! behind [`backend::EstimatorBackend`] without changing the app.

pub mod appearance;
pub mod backend;
pub mod cache;
pub mod derived;
pub mod export;
pub mod geometry;
pub mod noise;
pub mod pipeline;
pub mod pixels;
pub mod prep;
pub mod types;
mod util;

pub use appearance::{metallic, roughness};
pub use cache::MapStore;
pub use derived::{
    ao_maps, cavity, curvature, curvature_cavity, displacement, edge_wear, heightfield_ao,
    CurvatureOutput, DisplacementOutput,
};
pub use geometry::{geometry, height_from_luma, normals_from_height, GeometryOutput};
pub use pipeline::{
    run_pipeline, stage_keys, PipelineOutput, StageKey, StageOutput, BACKEND_ID, PIPELINE_VERSION,
};
pub use pixels::{hash_image, PbrImage};
pub use prep::{delit, prepare, DelitOutput, PrepOutput};
pub use types::{
    plan, AoSettings, CavitySettings, CurvatureSettings, DelitSettings, DisplacementSettings,
    EdgeWearSettings, HeightSettings, MapKind, MapToggles, MaterialClass, MetallicMode,
    MetallicSettings, NormalSettings, PbrSettings, Quality, RoughnessMode, RoughnessSettings,
    Stage,
};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum PbrError {
    #[error("image error: {0}")]
    Image(#[from] ::image::ImageError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Serialization(String),
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Message(String),
}

pub type Result<T> = std::result::Result<T, PbrError>;
