use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use glam::Vec3;
use image::{GrayImage, ImageBuffer, Luma, Rgb, RgbImage};

use crate::convention::{decode_normal, encode_normal, GreenChannel};
use crate::grid::Grid;
use crate::layers::Document;
use crate::{CoreError, Result};

/// Refuse project files larger than this before reading them into memory.
pub const MAX_PROJECT_BYTES: u64 = 1 << 30; // 1 GiB

/// Largest accepted image side for imported maps, masks and brushes.
pub const MAX_IMAGE_SIDE: u32 = 16_384;

/// Written as the first line of every legacy plain-RON project; its absence
/// means format 0 (everything saved before this marker existed). v2 archives
/// carry their own `format_version` in `manifest.json` instead.
pub const LEGACY_PROJECT_FORMAT: u32 = 1;
const FORMAT_PREFIX: &str = "// n-shade-project format=";

/// Decode an image with explicit limits so a hostile or corrupt file cannot
/// demand gigabytes before any of our own checks run.
pub fn open_image_limited(path: impl AsRef<Path>) -> Result<image::DynamicImage> {
    let mut reader = image::ImageReader::open(path)?.with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_SIDE);
    limits.max_image_height = Some(MAX_IMAGE_SIDE);
    limits.max_alloc = Some(1 << 30);
    reader.limits(limits);
    Ok(reader.decode()?)
}

/// Reject files written by a newer build than this one understands.
pub(crate) fn check_format_header(text: &str) -> Result<u32> {
    let Some(rest) = text.strip_prefix(FORMAT_PREFIX) else {
        return Ok(0); // legacy file
    };
    let line = rest.lines().next().unwrap_or("").trim();
    let format: u32 = line
        .parse()
        .map_err(|_| CoreError::Message(format!("unreadable project format marker '{line}'")))?;
    if format > LEGACY_PROJECT_FORMAT {
        return Err(CoreError::Message(format!(
            "this project was saved by a newer n-shade (format {format}); \
             this version reads formats up to {LEGACY_PROJECT_FORMAT}"
        )));
    }
    Ok(format)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BitDepth {
    Eight,
    Sixteen,
}

/// Load a grayscale image as a normalized height field.
pub fn load_height(path: impl AsRef<Path>) -> Result<Grid<f32>> {
    let img = open_image_limited(path)?;
    let w = img.width();
    let h = img.height();
    let luma = img.to_luma16();
    Ok(Grid::from_fn(w, h, |x, y| {
        luma.get_pixel(x, y)[0] as f32 / 65535.0
    }))
}

/// Load an alpha mask as a normalized height field. Images with an alpha
/// channel use that channel; grayscale/RGB images fall back to luminance.
pub fn load_alpha(path: impl AsRef<Path>) -> Result<Grid<f32>> {
    let img = open_image_limited(path)?;
    let w = img.width();
    let h = img.height();
    if img.color().has_alpha() {
        let rgba = img.to_rgba16();
        Ok(Grid::from_fn(w, h, |x, y| {
            rgba.get_pixel(x, y)[3] as f32 / 65535.0
        }))
    } else {
        let luma = img.to_luma16();
        Ok(Grid::from_fn(w, h, |x, y| {
            luma.get_pixel(x, y)[0] as f32 / 65535.0
        }))
    }
}

/// Load an RGB image as a canonical (OpenGL Y+) normal field.
pub fn load_normal(path: impl AsRef<Path>, convention: GreenChannel) -> Result<Grid<Vec3>> {
    let img = open_image_limited(path)?;
    let w = img.width();
    let h = img.height();
    let rgb = img.to_rgb16();
    Ok(Grid::from_fn(w, h, |x, y| {
        let p = rgb.get_pixel(x, y);
        decode_normal(
            Vec3::new(
                p[0] as f32 / 65535.0,
                p[1] as f32 / 65535.0,
                p[2] as f32 / 65535.0,
            ),
            convention,
        )
    }))
}

/// Load a mask image as normalized coverage.
pub fn load_mask(path: impl AsRef<Path>) -> Result<Grid<f32>> {
    load_height(path)
}

pub fn save_height(path: impl AsRef<Path>, height: &Grid<f32>, bit_depth: BitDepth) -> Result<()> {
    match bit_depth {
        BitDepth::Eight => {
            let img = GrayImage::from_fn(height.w, height.h, |x, y| {
                Luma([(height.get(x, y).clamp(0.0, 1.0) * 255.0).round() as u8])
            });
            img.save(path)?;
        }
        BitDepth::Sixteen => {
            let img: ImageBuffer<Luma<u16>, Vec<u16>> =
                ImageBuffer::from_fn(height.w, height.h, |x, y| {
                    Luma([(height.get(x, y).clamp(0.0, 1.0) * 65535.0).round() as u16])
                });
            img.save(path)?;
        }
    }
    Ok(())
}

/// Save an opaque 8-bit RGB material-ID map.
///
/// Colours are authored in sRGB and written verbatim: no colour-space
/// conversion and no interpolation, so neighbouring IDs never blend at their
/// shared edge.
pub fn save_id_map(path: impl AsRef<Path>, ids: &Grid<[f32; 3]>) -> Result<()> {
    let img = RgbImage::from_fn(ids.w, ids.h, |x, y| {
        let c = ids.get(x, y);
        Rgb([
            (c[0].clamp(0.0, 1.0) * 255.0).round() as u8,
            (c[1].clamp(0.0, 1.0) * 255.0).round() as u8,
            (c[2].clamp(0.0, 1.0) * 255.0).round() as u8,
        ])
    });
    img.save(path)?;
    Ok(())
}

pub fn save_normal(
    path: impl AsRef<Path>,
    normal: &Grid<Vec3>,
    convention: GreenChannel,
    bit_depth: BitDepth,
) -> Result<()> {
    match bit_depth {
        BitDepth::Eight => {
            let img = RgbImage::from_fn(normal.w, normal.h, |x, y| {
                let n = encode_normal(*normal.get(x, y), convention);
                Rgb([
                    (n.x.clamp(0.0, 1.0) * 255.0).round() as u8,
                    (n.y.clamp(0.0, 1.0) * 255.0).round() as u8,
                    (n.z.clamp(0.0, 1.0) * 255.0).round() as u8,
                ])
            });
            img.save(path)?;
        }
        BitDepth::Sixteen => {
            let img: ImageBuffer<Rgb<u16>, Vec<u16>> =
                ImageBuffer::from_fn(normal.w, normal.h, |x, y| {
                    let n = encode_normal(*normal.get(x, y), convention);
                    Rgb([
                        (n.x.clamp(0.0, 1.0) * 65535.0).round() as u16,
                        (n.y.clamp(0.0, 1.0) * 65535.0).round() as u16,
                        (n.z.clamp(0.0, 1.0) * 65535.0).round() as u16,
                    ])
                });
            img.save(path)?;
        }
    }
    Ok(())
}

/// Legacy plain-RON project format, still used for autosave snapshots and the
/// CLI. The versioned archive is written by [`crate::archive::save_project`].
///
/// The comment header is part of the RON whitespace, so older builds can still
/// read new files and parse-error line numbers stay correct.
pub fn save_project_ron(path: impl AsRef<Path>, doc: &Document) -> Result<()> {
    let body = ron::ser::to_string_pretty(doc, ron::ser::PrettyConfig::default())?;
    let text = format!("{FORMAT_PREFIX}{LEGACY_PROJECT_FORMAT}\n{body}");
    write_atomic(path, text.as_bytes())
}

pub fn load_project_ron(path: impl AsRef<Path>) -> Result<Document> {
    let path = path.as_ref();
    let len = std::fs::metadata(path)?.len();
    if len > MAX_PROJECT_BYTES {
        return Err(CoreError::Message(format!(
            "project file is {len} bytes; the limit is {MAX_PROJECT_BYTES}"
        )));
    }
    let text = std::fs::read_to_string(path)?;
    check_format_header(&text)?;
    let mut doc: Document = ron::from_str(&text)?;
    doc.validate()?;
    // Projects saved before cropped rasters stored baked pixels as a
    // document-sized grid. Give those layers their real bounds on load.
    doc.migrate_raster_sources();
    Ok(doc)
}

/// Write a file through a sibling temporary file, then rename it into place.
///
/// The bytes are flushed and synced to disk before the rename, so a crash, a
/// power loss or a full disk can leave a `*.tmp-*` file behind but never a
/// truncated destination: `path` always holds either the complete previous
/// version or the complete new one. The temporary file lives in the same
/// directory as the target so the rename stays on one volume and is atomic.
pub fn write_atomic(path: impl AsRef<Path>, bytes: &[u8]) -> Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let temp = temporary_sibling(path);
    let written = std::fs::File::create(&temp).and_then(|mut file| {
        file.write_all(bytes)?;
        file.sync_all()
    });
    if let Err(error) = written {
        let _ = std::fs::remove_file(&temp);
        return Err(error.into());
    }
    if let Err(error) = rename_replacing(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(error.into());
    }
    // The rename itself must survive a power cut on filesystems that journal
    // metadata separately from the file contents.
    sync_parent_dir(path);
    Ok(())
}

fn temporary_sibling(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let mut candidate = path.with_file_name(format!(".{name}.tmp-{}-{nanos}", std::process::id()));
    let mut suffix = 0;
    while candidate.exists() {
        suffix += 1;
        candidate = path.with_file_name(format!(
            ".{name}.tmp-{}-{nanos}-{suffix}",
            std::process::id()
        ));
    }
    candidate
}

/// Rename over an existing file, retrying brief sharing violations. Antivirus
/// scanners and indexers open freshly written files for a moment on Windows.
fn rename_replacing(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut delay = std::time::Duration::from_millis(20);
    let mut attempt = 0;
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(error) if attempt < 5 && transient_rename_error(&error) => {
                attempt += 1;
                std::thread::sleep(delay);
                delay *= 2;
            }
            Err(error) => return Err(error),
        }
    }
}

fn transient_rename_error(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::PermissionDenied
        // ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION, ERROR_LOCK_VIOLATION.
        || matches!(error.raw_os_error(), Some(5) | Some(32) | Some(33))
}

#[cfg(unix)]
fn sync_parent_dir(path: &Path) {
    if let Some(parent) = path.parent() {
        if let Ok(directory) = std::fs::File::open(parent) {
            let _ = directory.sync_all();
        }
    }
}

#[cfg(not(unix))]
fn sync_parent_dir(_path: &Path) {}

pub fn ensure_parent(path: impl AsRef<Path>) -> Result<()> {
    if let Some(parent) = path.as_ref().parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blend::BlendMode;
    use crate::layers::{Layer, LayerKind, VectorRole};
    use crate::path::{FillRule, Node, Path as VectorPath, ShapeSource, SubPath};

    #[test]
    fn id_map_png_keeps_every_colour_exact() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("id.png");
        let ids = Grid::from_fn(8, 4, |x, y| {
            if x < 4 {
                [1.0, 0.0, 0.5]
            } else if y < 2 {
                [0.0, 0.5, 1.0]
            } else {
                [0.0, 0.0, 0.0]
            }
        });
        save_id_map(&path, &ids).unwrap();
        let loaded = image::open(&path).unwrap().to_rgb8();
        assert_eq!(loaded.dimensions(), (8, 4));
        assert_eq!(loaded.get_pixel(0, 0).0, [255, 0, 128]);
        assert_eq!(loaded.get_pixel(4, 0).0, [0, 128, 255]);
        assert_eq!(loaded.get_pixel(4, 3).0, [0, 0, 0]);
        // No pixel is an average of two IDs: the edge stays hard.
        for pixel in loaded.pixels() {
            assert!(matches!(pixel.0, [255, 0, 128] | [0, 128, 255] | [0, 0, 0]));
        }
    }

    #[test]
    fn height_png_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("height.png");
        let h = Grid::from_fn(16, 16, |x, y| (x + y) as f32 / 30.0);
        save_height(&path, &h, BitDepth::Sixteen).unwrap();
        let loaded = load_height(&path).unwrap();
        assert_eq!(loaded.w, 16);
        assert_eq!(loaded.h, 16);
        for y in 0..16 {
            for x in 0..16 {
                assert!((*loaded.get(x, y) - *h.get(x, y)).abs() < 2.0 / 65535.0);
            }
        }
    }

    #[test]
    fn alpha_channel_is_preferred() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("alpha.png");
        let mut image = image::RgbaImage::new(4, 1);
        image.put_pixel(0, 0, image::Rgba([255, 0, 0, 0]));
        image.put_pixel(1, 0, image::Rgba([0, 255, 0, 128]));
        image.put_pixel(2, 0, image::Rgba([0, 0, 255, 255]));
        image.put_pixel(3, 0, image::Rgba([255, 255, 255, 255]));
        image.save(&path).unwrap();

        let loaded = load_alpha(&path).unwrap();
        assert!((*loaded.get(0, 0) - 0.0).abs() < 1e-4);
        assert!((*loaded.get(1, 0) - 128.0 / 255.0).abs() < 1e-3);
        assert!((*loaded.get(2, 0) - 1.0).abs() < 1e-4);
    }

    #[test]
    fn normal_png_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("normal.png");
        let n = Grid::from_fn(16, 16, |x, _| {
            Vec3::new(x as f32 / 15.0 - 0.5, 0.0, 0.8660254).normalize()
        });
        save_normal(&path, &n, GreenChannel::OpenGl, BitDepth::Sixteen).unwrap();
        let loaded = load_normal(&path, GreenChannel::OpenGl).unwrap();
        for y in 0..16 {
            for x in 0..16 {
                assert!((*loaded.get(x, y) - *n.get(x, y)).length() < 1e-4);
            }
        }
    }

    #[test]
    fn normal_png_round_trips_directx() {
        let dir = tempfile::tempdir().unwrap();
        let dx_path = dir.path().join("normal_dx.png");
        let gl_path = dir.path().join("normal_gl.png");
        let n = Grid::from_fn(8, 8, |x, _| {
            Vec3::new(0.0, (x as f32 / 7.0 - 0.5) * 0.8, 0.6).normalize()
        });
        save_normal(&dx_path, &n, GreenChannel::DirectX, BitDepth::Sixteen).unwrap();
        save_normal(&gl_path, &n, GreenChannel::OpenGl, BitDepth::Sixteen).unwrap();

        let loaded = load_normal(&dx_path, GreenChannel::DirectX).unwrap();
        for y in 0..8 {
            for x in 0..8 {
                assert!((*loaded.get(x, y) - *n.get(x, y)).length() < 1e-4);
            }
        }

        // The green byte is the only difference between the two files.
        let dx = image::open(&dx_path).unwrap().to_rgb8();
        let gl = image::open(&gl_path).unwrap().to_rgb8();
        let dx_g = dx.get_pixel(4, 4)[1] as u16;
        let gl_g = gl.get_pixel(4, 4)[1] as u16;
        assert!(dx_g.abs_diff(gl_g) > 8, "green was not flipped");
        assert_eq!(dx.get_pixel(4, 4)[0], gl.get_pixel(4, 4)[0]);
        assert_eq!(dx.get_pixel(4, 4)[2], gl.get_pixel(4, 4)[2]);
    }

    #[test]
    fn bmp_alpha_and_tiff_height_are_supported() {
        let dir = tempfile::tempdir().unwrap();
        let alpha_path = dir.path().join("mask.bmp");
        let height_path = dir.path().join("photo.tiff");
        let gray = GrayImage::from_fn(8, 8, |x, y| Luma([(x * 8 + y) as u8]));
        gray.save(&alpha_path).unwrap();
        gray.save(&height_path).unwrap();

        let alpha = load_alpha(&alpha_path).unwrap();
        assert_eq!((alpha.w, alpha.h), (8, 8));
        assert!(*alpha.get(4, 4) > 0.0);

        let height = load_height(&height_path).unwrap();
        assert_eq!((height.w, height.h), (8, 8));
        assert!(*height.get(4, 4) > 0.0);
    }

    #[test]
    fn project_ron_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("project.ron");
        let mut doc = Document::new(glam::UVec2::new(16, 16));
        doc.layers.push(Layer::fill("base", 0.5));
        save_project_ron(&path, &doc).unwrap();
        let loaded = load_project_ron(&path).unwrap();
        assert_eq!(loaded.size, doc.size);
        assert_eq!(loaded.layers.len(), 1);
        assert_eq!(loaded.layers[0].name, "base");
    }

    #[test]
    fn project_ron_round_trips_path_guides() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("guide.ron");
        let mut doc = Document::new(glam::UVec2::new(16, 16));
        let mut spine = VectorPath::new(FillRule::NonZero);
        let mut subpath = SubPath::new(false);
        subpath.nodes.push(Node::corner(glam::Vec2::new(2.0, 8.0)));
        subpath.nodes.push(Node::corner(glam::Vec2::new(14.0, 8.0)));
        spine.subpaths.push(subpath);
        let mut layer = Layer::shape("spine", ShapeSource::Path(spine));
        if let LayerKind::Shape(shape) = &mut layer.kind {
            shape.vector_role = VectorRole::Path;
        }
        doc.layers.push(layer);

        save_project_ron(&path, &doc).unwrap();
        let loaded = load_project_ron(&path).unwrap();
        assert!(loaded.layers[0].is_path_guide());
    }

    #[test]
    fn project_ron_round_trips_udim_tile_tags() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("udim.ron");
        let mut doc = Document::new(glam::UVec2::new(16, 16));
        let mut layer = Layer::fill("tile detail", 0.5);
        layer.tile = Some(1002);
        doc.layers.push(layer);
        doc.active_tile = 1012;

        save_project_ron(&path, &doc).unwrap();
        let loaded = load_project_ron(&path).unwrap();

        assert_eq!(loaded.layers[0].tile, Some(1002));
        // The canvas tile is runtime state; opening a project starts at 1001
        // instead of restoring whichever tile the last session showed.
        assert_eq!(loaded.active_tile, crate::BASE_UDIM_TILE);
    }

    #[test]
    fn project_ron_round_trips_groups_and_masks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("groups.ron");
        let mut doc = Document::new(glam::UVec2::new(16, 16));
        let mask = Layer::fill("mask", 0.0);
        let mut group = Layer::group("group", vec![Layer::fill("child", 0.5)]);
        group.mask_layer = Some(mask.id);
        group.blend = BlendMode::BooleanUnion;
        doc.layers.push(mask);
        doc.layers.push(group);

        save_project_ron(&path, &doc).unwrap();
        let loaded = load_project_ron(&path).unwrap();
        assert_eq!(loaded.layers.len(), 2);
        let loaded_group = &loaded.layers[1];
        assert!(loaded_group.is_group());
        assert_eq!(loaded_group.blend, BlendMode::BooleanUnion);
        assert_eq!(loaded_group.mask_layer, Some(loaded.layers[0].id));
        assert_eq!(loaded_group.children().unwrap().len(), 1);
        assert_eq!(loaded_group.children().unwrap()[0].name, "child");
    }

    #[test]
    fn old_full_document_rasters_gain_bounds_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old-raster.ron");
        let mut coverage = Grid::new(16, 16, 0.0);
        for y in 4..12 {
            for x in 4..12 {
                coverage.set(x, y, 1.0);
            }
        }
        let mut doc = Document::new(glam::UVec2::new(16, 16));
        doc.layers
            .push(Layer::shape("baked", ShapeSource::Raster(coverage)));
        save_project_ron(&path, &doc).unwrap();

        let loaded = load_project_ron(&path).unwrap();
        let LayerKind::Shape(shape) = &loaded.layers[0].kind else {
            panic!("expected a shape")
        };
        assert!(matches!(shape.source, ShapeSource::RasterRegion(_)));
        let bounds = shape.source.bounds().expect("migrated layer has bounds");
        assert_eq!(bounds.min, glam::Vec2::new(4.0, 4.0));
        assert_eq!(bounds.max, glam::Vec2::new(12.0, 12.0));
    }

    fn temporary_files(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".tmp-"))
            .collect()
    }

    #[test]
    fn atomic_writes_replace_the_old_file_without_leaving_temporaries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, b"old").unwrap();

        write_atomic(&path, b"new").unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert!(
            temporary_files(dir.path()).is_empty(),
            "left {:?}",
            temporary_files(dir.path())
        );
    }

    #[test]
    fn atomic_writes_create_missing_parent_folders() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join("nested")
            .join("deeper")
            .join("project.nsproj");

        write_atomic(&path, b"data").unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"data");
    }

    #[test]
    fn a_failed_atomic_write_keeps_the_destination_and_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        // A directory at the destination makes the final rename fail after the
        // temporary file has already been written.
        let path = dir.path().join("locked.nsproj");
        std::fs::create_dir(&path).unwrap();

        assert!(write_atomic(&path, b"data").is_err());

        assert!(path.is_dir(), "the destination must be untouched");
        assert!(
            temporary_files(dir.path()).is_empty(),
            "left {:?}",
            temporary_files(dir.path())
        );
    }

    #[test]
    fn project_save_writes_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deep").join("project.nsproj");

        save_project_ron(&path, &Document::new(glam::UVec2::splat(8))).unwrap();

        assert!(path.is_file());
        assert!(temporary_files(path.parent().unwrap()).is_empty());
    }

    #[test]
    fn project_files_carry_a_format_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("project.nsproj");
        save_project_ron(&path, &Document::new(glam::UVec2::splat(8))).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(FORMAT_PREFIX));
        assert_eq!(check_format_header(&text).unwrap(), LEGACY_PROJECT_FORMAT);
        assert!(load_project_ron(&path).is_ok());
    }

    #[test]
    fn newer_project_formats_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("future.nsproj");
        let body = ron::ser::to_string(&Document::new(glam::UVec2::splat(8))).unwrap();
        std::fs::write(&path, format!("{FORMAT_PREFIX}99\n{body}")).unwrap();

        let error = load_project_ron(&path).unwrap_err().to_string();
        assert!(error.contains("newer n-shade"), "{error}");
    }

    #[test]
    fn legacy_projects_without_a_header_still_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.nsproj");
        let body = ron::ser::to_string(&Document::new(glam::UVec2::splat(8))).unwrap();
        std::fs::write(&path, body).unwrap();

        assert!(load_project_ron(&path).is_ok());
    }

    #[test]
    fn oversized_project_files_are_refused_before_reading() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("huge.nsproj");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_PROJECT_BYTES + 1).unwrap();

        let error = load_project_ron(&path).unwrap_err().to_string();
        assert!(error.contains("limit"), "{error}");
    }

    #[test]
    fn load_rejects_a_mask_that_does_not_match_the_document() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad-mask.nsproj");
        let mut doc = Document::new(glam::UVec2::splat(8));
        let mut layer = Layer::fill("masked", 0.5);
        layer.mask = Some(Grid::new(2, 2, 1.0));
        doc.layers.push(layer);
        save_project_ron(&path, &doc).unwrap();

        assert!(load_project_ron(&path).is_err());
    }

    #[test]
    fn limited_image_decoding_rejects_absurd_dimensions() {
        let dir = tempfile::tempdir().unwrap();
        let tall = dir.path().join("tall.png");
        image::GrayImage::new(1, MAX_IMAGE_SIDE + 1)
            .save(&tall)
            .unwrap();
        assert!(open_image_limited(&tall).is_err());

        let small = dir.path().join("small.png");
        image::GrayImage::new(64, 64).save(&small).unwrap();
        assert!(open_image_limited(&small).is_ok());
    }

    #[test]
    fn every_showcase_project_still_loads() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../showcase");
        let Ok(entries) = std::fs::read_dir(dir) else {
            return; // showcase files are local-only in some checkouts
        };
        for entry in entries.flatten() {
            if entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "nsproj")
            {
                crate::archive::load_project(entry.path())
                    .unwrap_or_else(|error| panic!("{}: {error}", entry.path().display()));
            }
        }
    }
}
