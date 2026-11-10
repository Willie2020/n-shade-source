//! Export of generated maps, packed channels and displacement metadata.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use glam::Vec3;
use image::{GrayImage, ImageBuffer, Luma, Rgb, RgbImage};
use ns_core::convention::{encode_normal, GreenChannel};
use ns_core::Grid;

use crate::cache::MapStore;
use crate::pipeline::StageOutput;
use crate::pixels::srgb_encode;
use crate::types::{MapKind, MapToggles, PbrSettings, Stage};
use crate::{PbrError, Result};

#[derive(Clone, Copy, Debug)]
pub struct ExportOptions {
    pub sixteen_bit: bool,
    pub packed_gltf: bool,
    pub packed_orm: bool,
    pub packed_unity: bool,
    /// Long-edge export size. `0` keeps the working resolution.
    pub resolution: u32,
    /// Export-only map selection. `None` uses the pipeline's enabled maps.
    pub maps: Option<MapToggles>,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            sixteen_bit: true,
            packed_gltf: true,
            packed_orm: false,
            packed_unity: false,
            resolution: 0,
            maps: None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ExportReport {
    pub files: Vec<PathBuf>,
    pub errors: Vec<String>,
}

pub fn export_maps(
    directory: &Path,
    stem: &str,
    settings: &PbrSettings,
    store: &MapStore,
    options: &ExportOptions,
) -> Result<ExportReport> {
    std::fs::create_dir_all(directory)?;
    let mut report = ExportReport::default();
    let prefix = if stem.is_empty() { "texture" } else { stem };
    let selection = options.maps.unwrap_or(settings.maps);

    for kind in selection.enabled_iter() {
        let Some(output) = store.output(kind.stage()) else {
            report
                .errors
                .push(format!("{}: not generated", kind.label()));
            continue;
        };
        let path = directory.join(format!("{prefix}_{}.png", kind.suffix()));
        let saved = match kind {
            MapKind::Normal => output
                .normal()
                .ok_or_else(|| PbrError::Message("normal stage missing".into()))
                .and_then(|normal| {
                    let normal = scaled_normal(normal, options.resolution);
                    save_normal(
                        &path,
                        normal.as_ref(),
                        if settings.normal.direct_x {
                            GreenChannel::DirectX
                        } else {
                            GreenChannel::OpenGl
                        },
                        options.sixteen_bit,
                    )
                }),
            MapKind::Height => output
                .height()
                .ok_or_else(|| PbrError::Message("height stage missing".into()))
                .and_then(|height| {
                    let height = scaled_grid(height, options.resolution);
                    save_scalar(&path, height.as_ref(), options.sixteen_bit)
                }),
            MapKind::Displacement => output
                .displacement()
                .ok_or_else(|| PbrError::Message("displacement stage missing".into()))
                .and_then(|displacement| {
                    let map = scaled_grid(&displacement.map, options.resolution);
                    save_scalar(&path, map.as_ref(), options.sixteen_bit)?;
                    write_displacement_sidecar(directory, prefix, settings, displacement)?;
                    Ok(())
                }),
            MapKind::Delit => match output {
                StageOutput::Delit(delit) => {
                    let image = scaled_color(&delit.image, options.resolution);
                    save_color(&path, image.as_ref(), options.sixteen_bit)
                }
                _ => Err(PbrError::Message("delit stage missing".into())),
            },
            MapKind::Curvature => match output {
                StageOutput::Curvature(curvature) => {
                    let map = scaled_grid(&curvature.map, options.resolution);
                    save_scalar(&path, map.as_ref(), options.sixteen_bit)
                }
                _ => Err(PbrError::Message("curvature stage missing".into())),
            },
            MapKind::Ao
            | MapKind::Cavity
            | MapKind::EdgeWear
            | MapKind::Roughness
            | MapKind::Metallic => output
                .scalar()
                .ok_or_else(|| PbrError::Message(format!("{} stage missing", kind.label())))
                .and_then(|grid| {
                    let grid = scaled_grid(grid, options.resolution);
                    save_scalar(&path, grid.as_ref(), options.sixteen_bit)
                }),
        };
        match saved {
            Ok(()) => report.files.push(path),
            Err(error) => report.errors.push(format!("{}: {error}", kind.label())),
        }
    }

    let roughness = store.output(Stage::Roughness).and_then(StageOutput::scalar);
    let metallic = store.output(Stage::Metallic).and_then(StageOutput::scalar);
    let ao = store.output(Stage::Ao).and_then(StageOutput::scalar);
    let edge_wear = store.output(Stage::EdgeWear).and_then(StageOutput::scalar);
    let dimensions = roughness
        .or(metallic)
        .or(ao)
        .or(edge_wear)
        .map(|grid| (grid.w, grid.h));

    if let Some((w, h)) = dimensions {
        let (target_w, target_h) = target_size(w, h, options.resolution);
        let default_rough = Grid::new(target_w, target_h, 0.5);
        let default_metal = Grid::new(target_w, target_h, 0.0);
        let default_ao = Grid::new(target_w, target_h, 1.0);
        let default_wear = Grid::new(target_w, target_h, 0.0);
        let rough = scaled_or_default(roughness, &default_rough, options.resolution);
        let metal = scaled_or_default(metallic, &default_metal, options.resolution);
        let ao = scaled_or_default(ao, &default_ao, options.resolution);
        let wear = scaled_or_default(edge_wear, &default_wear, options.resolution);

        if options.packed_gltf {
            let path = directory.join(format!("{prefix}_metallic_roughness.png"));
            save_packed_gltf(&path, rough.as_ref(), metal.as_ref(), options.sixteen_bit)?;
            report.files.push(path);
        }
        if options.packed_orm {
            let path = directory.join(format!("{prefix}_orm.png"));
            save_packed_orm(
                &path,
                ao.as_ref(),
                rough.as_ref(),
                metal.as_ref(),
                options.sixteen_bit,
            )?;
            report.files.push(path);
        }
        if options.packed_unity {
            let path = directory.join(format!("{prefix}_mask.png"));
            save_packed_unity(
                &path,
                metal.as_ref(),
                ao.as_ref(),
                wear.as_ref(),
                rough.as_ref(),
            )?;
            report.files.push(path);
        }
    }

    Ok(report)
}

fn target_size(width: u32, height: u32, long_edge: u32) -> (u32, u32) {
    if long_edge == 0 || width.max(height) == long_edge {
        return (width, height);
    }
    let scale = long_edge as f32 / width.max(height) as f32;
    (
        (width as f32 * scale).round().max(1.0) as u32,
        (height as f32 * scale).round().max(1.0) as u32,
    )
}

fn scaled_grid<'a>(grid: &'a Grid<f32>, long_edge: u32) -> Cow<'a, Grid<f32>> {
    let (width, height) = target_size(grid.w, grid.h, long_edge);
    if (width, height) == (grid.w, grid.h) {
        Cow::Borrowed(grid)
    } else {
        Cow::Owned(grid.resized(width, height))
    }
}

fn scaled_normal<'a>(grid: &'a Grid<Vec3>, long_edge: u32) -> Cow<'a, Grid<Vec3>> {
    let (width, height) = target_size(grid.w, grid.h, long_edge);
    if (width, height) == (grid.w, grid.h) {
        Cow::Borrowed(grid)
    } else {
        Cow::Owned(grid.resized_normalized(width, height))
    }
}

fn scaled_color<'a>(
    image: &'a crate::pixels::PbrImage,
    long_edge: u32,
) -> Cow<'a, crate::pixels::PbrImage> {
    let (width, height) = target_size(image.w, image.h, long_edge);
    if (width, height) == (image.w, image.h) {
        Cow::Borrowed(image)
    } else {
        Cow::Owned(image.resized(width, height))
    }
}

fn scaled_or_default<'a>(
    grid: Option<&'a Grid<f32>>,
    default: &'a Grid<f32>,
    long_edge: u32,
) -> Cow<'a, Grid<f32>> {
    match grid {
        Some(grid) => scaled_grid(grid, long_edge),
        None => Cow::Borrowed(default),
    }
}

fn save_scalar(path: &Path, grid: &Grid<f32>, sixteen_bit: bool) -> Result<()> {
    if sixteen_bit {
        let image: ImageBuffer<Luma<u16>, Vec<u16>> =
            ImageBuffer::from_fn(grid.w, grid.h, |x, y| {
                Luma([(grid.get(x, y).clamp(0.0, 1.0) * 65535.0 + 0.5) as u16])
            });
        image.save(path)?;
    } else {
        let image = GrayImage::from_fn(grid.w, grid.h, |x, y| {
            Luma([(grid.get(x, y).clamp(0.0, 1.0) * 255.0 + 0.5) as u8])
        });
        image.save(path)?;
    }
    Ok(())
}

fn save_normal(
    path: &Path,
    normal: &Grid<Vec3>,
    convention: GreenChannel,
    sixteen_bit: bool,
) -> Result<()> {
    if sixteen_bit {
        let image: ImageBuffer<Rgb<u16>, Vec<u16>> =
            ImageBuffer::from_fn(normal.w, normal.h, |x, y| {
                let n = encode_normal(*normal.get(x, y), convention);
                Rgb([
                    (n.x.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16,
                    (n.y.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16,
                    (n.z.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16,
                ])
            });
        image.save(path)?;
    } else {
        let image = RgbImage::from_fn(normal.w, normal.h, |x, y| {
            let n = encode_normal(*normal.get(x, y), convention);
            Rgb([
                (n.x.clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
                (n.y.clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
                (n.z.clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
            ])
        });
        image.save(path)?;
    }
    Ok(())
}

fn save_color(path: &Path, color: &crate::pixels::PbrImage, sixteen_bit: bool) -> Result<()> {
    if sixteen_bit {
        let image: ImageBuffer<Rgb<u16>, Vec<u16>> =
            ImageBuffer::from_fn(color.w, color.h, |x, y| {
                let color = color.get(x, y);
                Rgb([
                    (srgb_encode(color.x) * 65535.0 + 0.5) as u16,
                    (srgb_encode(color.y) * 65535.0 + 0.5) as u16,
                    (srgb_encode(color.z) * 65535.0 + 0.5) as u16,
                ])
            });
        image.save(path)?;
    } else {
        let image = RgbImage::from_fn(color.w, color.h, |x, y| {
            let color = color.get(x, y);
            Rgb([
                (srgb_encode(color.x) * 255.0 + 0.5) as u8,
                (srgb_encode(color.y) * 255.0 + 0.5) as u8,
                (srgb_encode(color.z) * 255.0 + 0.5) as u8,
            ])
        });
        image.save(path)?;
    }
    Ok(())
}

fn save_packed_gltf(
    path: &Path,
    roughness: &Grid<f32>,
    metallic: &Grid<f32>,
    sixteen_bit: bool,
) -> Result<()> {
    let value = |grid: &Grid<f32>, x: u32, y: u32| grid.get(x, y).clamp(0.0, 1.0);
    if sixteen_bit {
        let image: ImageBuffer<Rgb<u16>, Vec<u16>> =
            ImageBuffer::from_fn(roughness.w, roughness.h, |x, y| {
                Rgb([
                    65535,
                    (value(roughness, x, y) * 65535.0 + 0.5) as u16,
                    (value(metallic, x, y) * 65535.0 + 0.5) as u16,
                ])
            });
        image.save(path)?;
    } else {
        let image = RgbImage::from_fn(roughness.w, roughness.h, |x, y| {
            Rgb([
                255,
                (value(roughness, x, y) * 255.0 + 0.5) as u8,
                (value(metallic, x, y) * 255.0 + 0.5) as u8,
            ])
        });
        image.save(path)?;
    }
    Ok(())
}

fn save_packed_orm(
    path: &Path,
    ao: &Grid<f32>,
    roughness: &Grid<f32>,
    metallic: &Grid<f32>,
    sixteen_bit: bool,
) -> Result<()> {
    let value = |grid: &Grid<f32>, x: u32, y: u32| grid.get(x, y).clamp(0.0, 1.0);
    if sixteen_bit {
        let image: ImageBuffer<Rgb<u16>, Vec<u16>> =
            ImageBuffer::from_fn(roughness.w, roughness.h, |x, y| {
                Rgb([
                    (value(ao, x, y) * 65535.0 + 0.5) as u16,
                    (value(roughness, x, y) * 65535.0 + 0.5) as u16,
                    (value(metallic, x, y) * 65535.0 + 0.5) as u16,
                ])
            });
        image.save(path)?;
    } else {
        let image = RgbImage::from_fn(roughness.w, roughness.h, |x, y| {
            Rgb([
                (value(ao, x, y) * 255.0 + 0.5) as u8,
                (value(roughness, x, y) * 255.0 + 0.5) as u8,
                (value(metallic, x, y) * 255.0 + 0.5) as u8,
            ])
        });
        image.save(path)?;
    }
    Ok(())
}

fn save_packed_unity(
    path: &Path,
    metallic: &Grid<f32>,
    ao: &Grid<f32>,
    edge_wear: &Grid<f32>,
    roughness: &Grid<f32>,
) -> Result<()> {
    let image: ImageBuffer<image::Rgba<u8>, Vec<u8>> =
        ImageBuffer::from_fn(roughness.w, roughness.h, |x, y| {
            image::Rgba([
                (metallic.get(x, y).clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
                (ao.get(x, y).clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
                (edge_wear.get(x, y).clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
                ((1.0 - roughness.get(x, y).clamp(0.0, 1.0)) * 255.0 + 0.5) as u8,
            ])
        });
    image.save(path)?;
    Ok(())
}

fn write_displacement_sidecar(
    directory: &Path,
    stem: &str,
    settings: &PbrSettings,
    displacement: &crate::derived::DisplacementOutput,
) -> Result<()> {
    let value = serde_json::json!({
        "midlevel": displacement.midlevel,
        "amount_m": displacement.amount_m,
        "height_scale_px": settings.height.height_scale_px,
        "convention": "0.5 = zero displacement unless midlevel differs",
    });
    let text = serde_json::to_string_pretty(&value)
        .map_err(|error| PbrError::Serialization(error.to_string()))?;
    std::fs::write(directory.join(format!("{stem}_displacement.json")), text)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::{StageKey, StageOutput};
    use std::collections::HashMap;
    use std::sync::Arc;

    #[test]
    fn exports_scalar_and_normal_maps() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = MapStore::with_budget(1024 * 1024);
        let height = Grid::from_fn(8, 8, |x, y| (x + y) as f32 / 14.0);
        let normal = ns_core::height::height_to_normal(&height, 4.0, ns_core::Wrap::Clamp, false);
        let mut current = HashMap::new();
        let key = StageKey([9; 32]);
        store.insert(
            key,
            Arc::new(StageOutput::Geometry(crate::geometry::GeometryOutput {
                height: height.clone(),
                normal,
            })),
        );
        current.insert(Stage::Geometry, key);
        store.set_current(&current);

        let settings = PbrSettings {
            maps: crate::types::MapToggles {
                normal: true,
                height: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let report = export_maps(
            directory.path(),
            "sample",
            &settings,
            &store,
            &ExportOptions::default(),
        )
        .unwrap();
        assert_eq!(report.errors.len(), 0);
        assert!(directory.path().join("sample_normal.png").exists());
        assert!(directory.path().join("sample_height.png").exists());
    }

    #[test]
    fn export_resolution_resizes_normal_and_height() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = MapStore::with_budget(1024 * 1024);
        let height = Grid::from_fn(8, 8, |x, y| (x + y) as f32 / 14.0);
        let normal = ns_core::height::height_to_normal(&height, 4.0, ns_core::Wrap::Clamp, false);
        let mut current = HashMap::new();
        let key = StageKey([10; 32]);
        store.insert(
            key,
            Arc::new(StageOutput::Geometry(crate::geometry::GeometryOutput {
                height: height.clone(),
                normal,
            })),
        );
        current.insert(Stage::Geometry, key);
        store.set_current(&current);

        let settings = PbrSettings {
            maps: crate::types::MapToggles {
                normal: true,
                height: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let options = ExportOptions {
            resolution: 16,
            ..Default::default()
        };
        let report = export_maps(directory.path(), "scaled", &settings, &store, &options).unwrap();
        assert!(report.errors.is_empty(), "{:?}", report.errors);

        let normal = image::open(directory.path().join("scaled_normal.png")).unwrap();
        assert_eq!((normal.width(), normal.height()), (16, 16));
        let height = image::open(directory.path().join("scaled_height.png")).unwrap();
        assert_eq!((height.width(), height.height()), (16, 16));
    }
}
