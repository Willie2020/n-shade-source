//! Portable v2 project archive and legacy v1 fallback.
//!
//! A v2 `.nsproj` is a ZIP file containing:
//!
//! ```text
//! manifest.json
//! project.ron
//! document.ron
//! meshes/*.nsmesh
//! fonts/*
//! environments/*
//! ```
//!
//! A legacy v1 `.nsproj` is a plain RON `Document`. Every loader entry point
//! accepts both. No derived map is ever written to the archive.

use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::layers::Document;
use crate::mesh_asset::MeshAsset;
use crate::project::{layer_id_string, ProjectRoot, PROJECT_FORMAT_VERSION};
use crate::{CoreError, Result};

pub const ARCHIVE_FORMAT: &str = "n-shade/project";
pub const MANIFEST_NAME: &str = "manifest.json";
pub const PROJECT_NAME: &str = "project.ron";
pub const DEFAULT_DOCUMENT_NAME: &str = "document.ron";
const MAX_ENTRY_BYTES: u64 = 1024 * 1024 * 1024;

/// Asset kind recorded in the manifest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    Mesh,
    Font,
    Environment,
    SourceMesh,
}

impl AssetKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Mesh => "mesh",
            Self::Font => "font",
            Self::Environment => "environment",
            Self::SourceMesh => "source_mesh",
        }
    }
}

/// One embedded asset supplied to [`save_project`].
#[derive(Clone, Debug)]
pub struct ProjectAsset {
    pub id: String,
    pub kind: AssetKind,
    /// Archive-relative path. Use forward slashes.
    pub path: String,
    pub bytes: Vec<u8>,
}

/// Manifest entry for one embedded asset.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssetEntry {
    pub id: String,
    pub kind: AssetKind,
    pub path: String,
    /// `blake3:<hex>` over the uncompressed bytes.
    pub hash: String,
    pub bytes: u64,
    #[serde(default = "default_codec")]
    pub codec: String,
}

fn default_codec() -> String {
    "none".into()
}

/// `manifest.json` payload.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub format: String,
    pub format_version: u32,
    pub min_reader_version: u32,
    #[serde(default)]
    pub app_version: String,
    #[serde(default)]
    pub created_utc: String,
    #[serde(default)]
    pub modified_utc: String,
    #[serde(default = "default_project_name")]
    pub root: String,
    #[serde(default = "default_document_name")]
    pub document: String,
    #[serde(default)]
    pub assets: Vec<AssetEntry>,
}

fn default_project_name() -> String {
    PROJECT_NAME.into()
}

fn default_document_name() -> String {
    DEFAULT_DOCUMENT_NAME.into()
}

/// One mesh loaded from an archive.
#[derive(Clone, Debug)]
pub struct LoadedMesh {
    pub entry: crate::project::MeshEntry,
    pub asset: MeshAsset,
}

/// One custom font loaded from an archive.
#[derive(Clone, Debug)]
pub struct LoadedFont {
    pub original_path: String,
    pub file_name: String,
    pub bytes: Vec<u8>,
}

/// One environment loaded from an archive.
#[derive(Clone, Debug)]
pub struct LoadedEnvironment {
    pub id: String,
    pub file_name: String,
    pub bytes: Vec<u8>,
}

/// Result of opening either a v1 or v2 project.
#[derive(Clone, Debug)]
pub struct LoadedProject {
    pub project: ProjectRoot,
    pub document: Document,
    pub meshes: Vec<LoadedMesh>,
    pub fonts: Vec<LoadedFont>,
    pub environments: Vec<LoadedEnvironment>,
    /// True when the source file was a legacy plain-RON `Document`.
    pub legacy: bool,
    pub warnings: Vec<String>,
}

impl LoadedProject {
    pub fn mesh(&self, id: &str) -> Option<&LoadedMesh> {
        self.meshes.iter().find(|mesh| mesh.entry.id == id)
    }

    pub fn environment(&self, id: &str) -> Option<&LoadedEnvironment> {
        self.environments
            .iter()
            .find(|environment| environment.id == id)
    }
}

/// Write a v2 archive atomically.
pub fn save_project(
    path: impl AsRef<Path>,
    project: &ProjectRoot,
    document: &Document,
    assets: &[ProjectAsset],
) -> Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }

    let mut seen_ids = HashSet::new();
    let mut seen_paths = HashSet::new();
    for asset in assets {
        if asset.id.trim().is_empty() {
            return Err(CoreError::Message("asset has an empty id".into()));
        }
        if !seen_ids.insert(asset.id.clone()) {
            return Err(CoreError::Message(format!(
                "duplicate asset id {}",
                asset.id
            )));
        }
        if !seen_paths.insert(asset.path.clone()) {
            return Err(CoreError::Message(format!(
                "duplicate asset path {}",
                asset.path
            )));
        }
        validate_archive_path(&asset.path)?;
    }

    let timestamp = unix_timestamp();
    let manifest = Manifest {
        format: ARCHIVE_FORMAT.into(),
        format_version: PROJECT_FORMAT_VERSION,
        min_reader_version: 2,
        app_version: project.app_version.clone(),
        created_utc: timestamp.clone(),
        modified_utc: timestamp,
        root: PROJECT_NAME.into(),
        document: project.document.clone(),
        assets: assets
            .iter()
            .map(|asset| AssetEntry {
                id: asset.id.clone(),
                kind: asset.kind,
                path: asset.path.clone(),
                hash: format!("blake3:{}", blake3::hash(&asset.bytes).to_hex()),
                bytes: asset.bytes.len() as u64,
                codec: default_codec(),
            })
            .collect(),
    };

    let manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| CoreError::Message(format!("manifest serialization failed: {error}")))?;
    let project_bytes =
        ron::ser::to_string_pretty(project, ron::ser::PrettyConfig::default())?.into_bytes();
    let document_bytes = ron::ser::to_string(document)?.into_bytes();

    let temporary = temporary_sibling(path);
    {
        let file = std::fs::File::create(&temporary)?;
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);

        writer
            .start_file(MANIFEST_NAME, options)
            .map_err(zip_error)?;
        writer.write_all(&manifest_bytes)?;

        writer
            .start_file(PROJECT_NAME, options)
            .map_err(zip_error)?;
        writer.write_all(&project_bytes)?;

        let document_name = if manifest.document.trim().is_empty() {
            DEFAULT_DOCUMENT_NAME
        } else {
            manifest.document.as_str()
        };
        validate_archive_path(document_name)?;
        writer
            .start_file(document_name, options)
            .map_err(zip_error)?;
        writer.write_all(&document_bytes)?;

        for asset in assets {
            writer
                .start_file(asset.path.as_str(), options)
                .map_err(zip_error)?;
            writer.write_all(&asset.bytes)?;
        }

        writer.finish().map_err(zip_error)?;
    }

    // Windows does not replace an existing file with rename. Remove the old
    // target first, then rename the complete temporary file. The temporary is
    // only removed after the new archive is fully written.
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    std::fs::rename(&temporary, path)?;
    Ok(())
}

/// Load either a v2 archive or a legacy v1 plain-RON document.
pub fn load_project(path: impl AsRef<Path>) -> Result<LoadedProject> {
    let path = path.as_ref();
    let len = std::fs::metadata(path)?.len();
    if len > crate::io::MAX_PROJECT_BYTES {
        return Err(CoreError::Message(format!(
            "project file is {len} bytes; the limit is {}",
            crate::io::MAX_PROJECT_BYTES
        )));
    }
    let bytes = std::fs::read(path)?;
    if bytes.starts_with(b"PK\x03\x04") {
        load_archive(&bytes)
    } else {
        load_legacy(&bytes)
    }
}

fn load_legacy(bytes: &[u8]) -> Result<LoadedProject> {
    let text = std::str::from_utf8(bytes)
        .map_err(|error| CoreError::Message(format!("project is not UTF-8: {error}")))?;
    crate::io::check_format_header(text)?;
    let mut document: Document = ron::from_str(text)?;
    document.validate()?;
    document.migrate_raster_sources();
    Ok(LoadedProject {
        project: ProjectRoot::default(),
        document,
        meshes: Vec::new(),
        fonts: Vec::new(),
        environments: Vec::new(),
        legacy: true,
        warnings: vec![
            "Legacy v1 project: mesh, editor, viewer and lightmap inputs were not stored.".into(),
        ],
    })
}

fn load_archive(bytes: &[u8]) -> Result<LoadedProject> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes.to_vec())).map_err(zip_error)?;
    let manifest_bytes = read_entry(&mut archive, MANIFEST_NAME)?;
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| CoreError::Message(format!("manifest parse failed: {error}")))?;
    if manifest.format != ARCHIVE_FORMAT {
        return Err(CoreError::Message(format!(
            "unsupported project format {}",
            manifest.format
        )));
    }
    if manifest.format_version > PROJECT_FORMAT_VERSION {
        return Err(CoreError::Message(format!(
            "project format {} is newer than this build supports ({})",
            manifest.format_version, PROJECT_FORMAT_VERSION
        )));
    }

    let project_name = if manifest.root.trim().is_empty() {
        PROJECT_NAME
    } else {
        manifest.root.as_str()
    };
    let project_bytes = read_entry(&mut archive, project_name)?;
    let project_text = std::str::from_utf8(&project_bytes)
        .map_err(|error| CoreError::Message(format!("project.ron is not UTF-8: {error}")))?;
    let mut project: ProjectRoot = ron::from_str(project_text)?;

    let document_name = if project.document.trim().is_empty() {
        if manifest.document.trim().is_empty() {
            DEFAULT_DOCUMENT_NAME
        } else {
            manifest.document.as_str()
        }
    } else {
        project.document.as_str()
    };
    let document_bytes = read_entry(&mut archive, document_name)?;
    let document_text = std::str::from_utf8(&document_bytes)
        .map_err(|error| CoreError::Message(format!("document.ron is not UTF-8: {error}")))?;
    let mut document: Document = ron::from_str(document_text)?;
    document.validate()?;
    document.migrate_raster_sources();

    let mut warnings = Vec::new();
    let mut assets: HashMap<String, Vec<u8>> = HashMap::new();
    for entry in &manifest.assets {
        match read_entry(&mut archive, &entry.path) {
            Ok(bytes) => {
                if bytes.len() as u64 != entry.bytes {
                    return Err(CoreError::Message(format!(
                        "asset {} has {} bytes, expected {}",
                        entry.id,
                        bytes.len(),
                        entry.bytes
                    )));
                }
                verify_hash(entry, &bytes)?;
                assets.insert(entry.id.clone(), bytes);
            }
            Err(error) => {
                if matches!(entry.kind, AssetKind::Mesh) {
                    return Err(CoreError::Message(format!(
                        "required mesh asset {} is missing: {error}",
                        entry.id
                    )));
                }
                warnings.push(format!(
                    "Optional asset {} ({}) is missing: {error}",
                    entry.id,
                    entry.kind.label()
                ));
            }
        }
    }

    let mut meshes = Vec::new();
    for entry in &project.meshes {
        let Some(bytes) = assets.get(&entry.asset) else {
            warnings.push(format!("Mesh {} has no embedded asset", entry.id));
            continue;
        };
        match MeshAsset::decode(bytes) {
            Ok(asset) => meshes.push(LoadedMesh {
                entry: entry.clone(),
                asset,
            }),
            Err(error) => {
                return Err(CoreError::Message(format!(
                    "mesh {} is corrupt: {error}",
                    entry.id
                )));
            }
        }
    }

    let mut fonts = Vec::new();
    for entry in &project.fonts {
        let Some(bytes) = assets.get(&entry.asset) else {
            warnings.push(format!(
                "Custom font {} is not embedded; text layers will use a fallback",
                entry.original_path
            ));
            continue;
        };
        let file_name = manifest
            .assets
            .iter()
            .find(|asset| asset.id == entry.asset)
            .and_then(|asset| asset.path.rsplit('/').next())
            .unwrap_or("font.ttf")
            .to_string();
        fonts.push(LoadedFont {
            original_path: entry.original_path.clone(),
            file_name,
            bytes: bytes.clone(),
        });
    }

    let mut environments = Vec::new();
    for entry in &manifest.assets {
        if entry.kind != AssetKind::Environment {
            continue;
        }
        let Some(bytes) = assets.get(&entry.id) else {
            continue;
        };
        environments.push(LoadedEnvironment {
            id: entry.id.clone(),
            file_name: entry
                .path
                .rsplit('/')
                .next()
                .unwrap_or("environment.hdr")
                .to_string(),
            bytes: bytes.clone(),
        });
    }

    if project.format_version == 0 {
        project.format_version = manifest.format_version;
    }

    Ok(LoadedProject {
        project,
        document,
        meshes,
        fonts,
        environments,
        legacy: false,
        warnings,
    })
}

fn read_entry<R: Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    name: &str,
) -> Result<Vec<u8>> {
    validate_archive_path(name)?;
    let mut file = archive
        .by_name(name)
        .map_err(|error| CoreError::Message(format!("missing archive entry {name}: {error}")))?;
    let size = file.size();
    if size > MAX_ENTRY_BYTES {
        return Err(CoreError::Message(format!(
            "archive entry {name} is larger than the safety limit"
        )));
    }
    let mut bytes = Vec::with_capacity(size.min(16 * 1024 * 1024) as usize);
    file.read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_ENTRY_BYTES {
        return Err(CoreError::Message(format!(
            "archive entry {name} exceeded the safety limit"
        )));
    }
    Ok(bytes)
}

fn verify_hash(entry: &AssetEntry, bytes: &[u8]) -> Result<()> {
    if entry.hash.trim().is_empty() {
        return Ok(());
    }
    let actual = format!("blake3:{}", blake3::hash(bytes).to_hex());
    if entry.hash != actual {
        return Err(CoreError::Message(format!(
            "asset {} failed hash verification",
            entry.id
        )));
    }
    Ok(())
}

fn validate_archive_path(path: &str) -> Result<()> {
    if path.trim().is_empty() {
        return Err(CoreError::Message("archive path is empty".into()));
    }
    if path.starts_with('/') || path.starts_with('\\') {
        return Err(CoreError::Message(format!(
            "archive path is absolute: {path}"
        )));
    }
    if path.contains('\\') {
        return Err(CoreError::Message(format!(
            "archive path uses backslashes: {path}"
        )));
    }
    if path.as_bytes().get(1) == Some(&b':') {
        return Err(CoreError::Message(format!(
            "archive path has a drive letter: {path}"
        )));
    }
    for component in path.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(CoreError::Message(format!(
                "archive path is not safe: {path}"
            )));
        }
    }
    Ok(())
}

fn zip_error(error: zip::result::ZipError) -> CoreError {
    CoreError::Message(format!("project archive error: {error}"))
}

fn temporary_sibling(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("project.nsproj");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    path.with_file_name(format!(".{name}.tmp-{}-{nanos}", std::process::id()))
}

fn unix_timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs().to_string())
        .unwrap_or_else(|_| "0".into())
}

/// A stable string for diagnostics.
pub fn archive_summary(project: &ProjectRoot) -> String {
    let mut text = format!(
        "format v{} · {} mesh(es) · {} font(s)",
        project.format_version,
        project.meshes.len(),
        project.fonts.len()
    );
    if let Some(lightmap) = &project.lightmap {
        text.push_str(&format!(" · lightmap {}px", lightmap.settings.resolution));
    }
    if let Some(first) = project.meshes.first() {
        text.push_str(&format!(" · mesh {}", first.id));
    }
    text
}

/// Layer IDs are stored as strings in `project.ron`; this helper keeps the
/// conversion in one place for callers that need to build selections.
pub fn layer_ids(values: &[crate::layers::LayerId]) -> Vec<String> {
    values.iter().copied().map(layer_id_string).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layers::Document;
    use crate::project::ProjectRoot;
    use glam::UVec2;

    #[test]
    fn legacy_document_still_loads() {
        let document = Document::new(UVec2::splat(32));
        let text = ron::ser::to_string(&document).unwrap();
        let bytes = text.into_bytes();
        let loaded = load_legacy(&bytes).unwrap();
        assert!(loaded.legacy);
        assert_eq!(loaded.document.size, UVec2::splat(32));
        assert!(!loaded.warnings.is_empty());
    }

    #[test]
    fn archive_round_trips_project_metadata() {
        let directory =
            std::env::temp_dir().join(format!("n-shade-archive-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("project.nsproj");
        let document = Document::new_with_base(UVec2::splat(32));
        let mut project = ProjectRoot::new("test");
        project.editor.active_tile = 1002;
        save_project(&path, &project, &document, &[]).unwrap();
        let loaded = load_project(&path).unwrap();
        assert!(!loaded.legacy);
        assert_eq!(loaded.project.editor.active_tile, 1002);
        assert_eq!(loaded.document.size, UVec2::splat(32));
        let _ = std::fs::remove_dir_all(&directory);
    }
}
