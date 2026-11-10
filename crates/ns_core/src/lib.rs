//! Core CPU reference implementation for N-shade.
//!
//! Everything in this crate is engine-agnostic. Bevy lives in `ns_app`; the
//! algorithms here are the correctness oracle for the GPU implementation.

pub mod archive;
pub mod blend;
pub mod brush;
pub mod cable;
pub mod convention;
pub mod demo;
pub mod derived;
pub mod filter;
pub mod grid;
pub mod height;
pub mod io;
pub mod layers;
pub mod mask;
pub mod mesh_asset;
pub mod path;
pub mod project;
pub mod sdf;
pub mod svg;
pub mod text;

pub use blend::{blend, derivative_blend, rnm, slope, with_slope_opacity, BlendMode, BooleanOp};
// `CancelToken` and `eval_pool` are defined above so they are available without
// an extra module.
pub use cable::{cable_layer, CableCap, CableMeta};
pub use convention::{decode_normal, encode_normal, GreenChannel};
pub use demo::{demo_document, demo_text_document, stress_document};
pub use grid::{Grid, Wrap};
pub use height::{height_to_normal, normal_to_height, NormalToHeightOptions};
pub use layers::{
    BooleanOperand, Document, EvalCache, Layer, LayerId, LayerKind, PhotoHeightLayer, ShapeLayer,
    ShapeStyle, VectorOp, VectorRole, BASE_UDIM_TILE, DEFAULT_EVAL_CACHE_BUDGET,
};
pub use mask::{CurvatureMode, GeneratorKind, MaskGenerator, MaskSettings};
pub use path::{
    boolean_paths, Bounds, FillRule, Node, NodeKind, Path, PathBoolean, Primitive, RasterRegion,
    ShapeSource, SubPath,
};
pub use sdf::{BevelParams, Profile};
pub use svg::{load_svg, parse_svg, SvgShape};
pub use text::{BuiltinFont, FontSource, TextAlign, TextSource};

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use thiserror::Error;

/// Dedicated thread pool for CPU document evaluation.
///
/// Bevy's async executor and the global `rayon` pool are shared with rendering,
/// asset loading and editor work. Running the evaluator's parallel kernels in
/// their own pool keeps a long evaluation from starving those jobs while the
/// UI stays responsive. `install` also routes nested rayon operations (for
/// example `Grid::from_fn_par`) into this pool.
/// Cooperative cancellation flag shared with a running evaluation.
///
/// Checks happen at layer boundaries and before/after the tree evaluator, so a
/// superseded job stops without waiting for the rest of the stack. A single
/// very large layer still finishes its current kernel.
#[derive(Clone, Debug, Default)]
pub struct CancelToken {
    cancelled: Arc<AtomicBool>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

pub fn eval_pool() -> &'static rayon::ThreadPool {
    static POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();
    POOL.get_or_init(|| {
        // Leave at least one core for the UI, render and Bevy's own pools.
        let threads = std::thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(4)
            .saturating_sub(2)
            .max(1);
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|index| format!("ns-eval-{index}"))
            .build()
            .or_else(|_| {
                // OS thread creation can fail under tight limits. A single
                // thread is slower but keeps the editor usable.
                rayon::ThreadPoolBuilder::new()
                    .num_threads(1)
                    .thread_name(|_| "ns-eval".into())
                    .build()
            })
            .expect("build ns_core evaluation pool")
    })
}

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("image error: {0}")]
    Image(#[from] image::ImageError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Ron(#[from] ron::Error),
    #[error("project parse error: {0}")]
    RonParse(#[from] ron::error::SpannedError),
    #[error("invalid dimensions: {0}x{1}")]
    InvalidDimensions(u32, u32),
    #[error("{0}")]
    Message(String),
}

pub type Result<T> = std::result::Result<T, CoreError>;

#[cfg(test)]
mod tests {
    use super::eval_pool;
    use rayon::prelude::*;

    #[test]
    fn cancel_token_trips_once() {
        let token = super::CancelToken::new();
        assert!(!token.is_cancelled());
        token.cancel();
        assert!(token.is_cancelled());
        token.clone().cancel();
        assert!(token.is_cancelled());
    }

    #[test]
    fn eval_pool_runs_nested_parallel_work() {
        let pool = eval_pool();
        assert!(pool.current_num_threads() >= 1);
        let sum: i64 = pool.install(|| (0..100_i64).into_par_iter().sum());
        assert_eq!(sum, 4_950);
    }
}
