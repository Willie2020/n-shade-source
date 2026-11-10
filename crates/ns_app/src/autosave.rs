//! Crash-safe autosave snapshots.
//!
//! While a project has unsaved changes, a recovery snapshot is written every
//! [`INTERVAL_SECS`] seconds into a per-project folder next to the project
//! (`demo.nsproj` → `demo.autosave/`). Untitled work goes to the app's recovery
//! folder (`%LOCALAPPDATA%/n-shade/autosave` on Windows).
//!
//! Every snapshot is an ordinary `.nsproj` file written through
//! [`ns_core::io::write_atomic`]: the document is serialized to a sibling
//! temporary file, flushed and synced, and only then renamed into place. A
//! crash, power cut or full disk can leave a `*.tmp-*` file behind, but it can
//! never truncate a snapshot or the project itself. Old snapshots rotate out by
//! count (the newest [`MAX_BACKUPS`] are kept).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bevy::prelude::*;
use bevy::tasks::{block_on, poll_once, AsyncComputeTaskPool, Task};
use serde_json::Value;

use crate::app::{DocumentRes, EditorState};

/// How often unsaved work is snapshotted while autosave is on.
pub const INTERVAL_SECS: f32 = 60.0;
/// Fewest snapshots that can be kept. `0` would defeat the point of rotating.
pub const MIN_BACKUPS: usize = 1;
/// Upper bound for the "Backups to keep" control.
pub const MAX_BACKUPS: usize = 50;
pub const DEFAULT_BACKUPS: usize = 5;

/// Snapshot folder name suffix: `demo.nsproj` → `demo.autosave/`.
const FOLDER_SUFFIX: &str = ".autosave";

#[derive(Resource)]
pub struct AutosaveRuntime {
    timer: Timer,
    /// The write in flight, if any. Polled once per frame so the UI thread never
    /// waits on the disk.
    job: Option<Task<Result<SnapshotOutcome, String>>>,
    was_enabled: bool,
    /// Last settings values written to disk, so a change can be detected and
    /// persisted once per frame instead of every frame.
    saved_settings: Option<(bool, usize)>,
}

impl Default for AutosaveRuntime {
    fn default() -> Self {
        Self {
            timer: Timer::from_seconds(INTERVAL_SECS, TimerMode::Repeating),
            job: None,
            was_enabled: false,
            saved_settings: None,
        }
    }
}

struct SnapshotOutcome {
    path: PathBuf,
}

/// Writes recovery snapshots for unsaved work and persists the settings.
pub fn autosave_system(
    time: Res<Time>,
    doc: Res<DocumentRes>,
    mut state: ResMut<EditorState>,
    mut autosave: ResMut<AutosaveRuntime>,
) {
    autosave.poll(&mut state);

    if !state.suppress_autosave {
        let current = (state.autosave_enabled, state.autosave_backups);
        match autosave.saved_settings {
            // First frame: remember what the startup settings were rather than
            // writing them back to disk immediately.
            None => autosave.saved_settings = Some(current),
            Some(previous) if previous != current => {
                autosave.saved_settings = Some(current);
                if let Err(error) = save_settings(current.0, current.1) {
                    state.autosave_error =
                        Some(format!("Could not save autosave settings: {error}"));
                }
                if previous.1 != current.1 {
                    // Honor a lower backup count immediately instead of waiting
                    // for the next snapshot.
                    let dir = autosave_dir(state.project_path.as_deref());
                    let stem = snapshot_stem(state.project_path.as_deref());
                    let _ = prune(&dir, &stem, current.1);
                }
            }
            Some(_) => {}
        }
    }

    if state.suppress_autosave || !state.autosave_enabled {
        autosave.was_enabled = false;
        return;
    }
    if !autosave.was_enabled {
        // Turning autosave on starts a fresh interval instead of firing at once.
        autosave.was_enabled = true;
        autosave.timer.reset();
    }

    autosave.timer.tick(time.delta());
    if autosave.job.is_some() || !autosave.timer.just_finished() || !state.has_unsaved_changes() {
        return;
    }

    autosave.timer.reset();
    let dir = autosave_dir(state.project_path.as_deref());
    let stem = snapshot_stem(state.project_path.as_deref());
    let keep = state.autosave_backups.clamp(MIN_BACKUPS, MAX_BACKUPS);
    // `SharedDocument` clones are O(1): the worker serializes the same
    // immutable revision the editor is showing, without copying every grid on
    // the UI thread. The next edit will copy-on-write as usual.
    let snapshot = doc.0.clone();
    autosave.job = Some(
        AsyncComputeTaskPool::get()
            .spawn(async move { write_snapshot(&dir, &stem, keep, &snapshot) }),
    );
}

impl AutosaveRuntime {
    fn poll(&mut self, state: &mut EditorState) {
        let Some(mut task) = self.job.take() else {
            return;
        };
        let Some(result) = block_on(poll_once(&mut task)) else {
            // Still writing; check again next frame.
            self.job = Some(task);
            return;
        };
        match result {
            Ok(outcome) => {
                state.autosave_error = None;
                state.autosave_status = format!(
                    "Last autosave: {}",
                    outcome
                        .path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("snapshot.nsproj")
                );
            }
            Err(error) => {
                state.autosave_error = Some(error.clone());
                state.status = format!("Autosave failed: {error}");
            }
        }
    }
}

/// Folder that receives this project's snapshots.
pub fn autosave_dir(project_path: Option<&Path>) -> PathBuf {
    match project_path {
        Some(path) => {
            let stem = snapshot_stem(project_path);
            let parent = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty());
            match parent {
                Some(parent) => parent.join(format!("{stem}{FOLDER_SUFFIX}")),
                None => PathBuf::from(format!("{stem}{FOLDER_SUFFIX}")),
            }
        }
        None => recovery_root(),
    }
}

/// File-name stem used by this project's snapshots.
pub fn snapshot_stem(project_path: Option<&Path>) -> String {
    project_path
        .and_then(|path| path.file_stem())
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| "untitled".to_string())
}

/// Newest snapshot for this stem, if any.
pub fn latest_snapshot(dir: &Path, stem: &str) -> io::Result<Option<PathBuf>> {
    Ok(snapshot_files(dir, stem)?.pop())
}

/// Delete all but the newest `keep` snapshots for `stem`. Unrelated files are
/// left alone.
pub fn prune(dir: &Path, stem: &str, keep: usize) -> io::Result<usize> {
    let files = snapshot_files(dir, stem)?;
    let keep = keep.clamp(MIN_BACKUPS, MAX_BACKUPS);
    let remove = files.len().saturating_sub(keep);
    let mut removed = 0;
    for path in files.into_iter().take(remove) {
        if fs::remove_file(path).is_ok() {
            removed += 1;
        }
    }
    remove_stale_temporaries(dir);
    Ok(removed)
}

fn write_snapshot(
    dir: &Path,
    stem: &str,
    keep: usize,
    doc: &ns_core::Document,
) -> Result<SnapshotOutcome, String> {
    fs::create_dir_all(dir)
        .map_err(|error| format!("could not create {}: {error}", dir.display()))?;
    let path = snapshot_path(dir, stem);
    ns_core::io::save_project_ron(&path, doc)
        .map_err(|error| format!("could not write {}: {error}", path.display()))?;
    // Rotation is best effort. Failing to delete an old snapshot must not make
    // a successfully written one look like a failure.
    let _ = prune(dir, stem, keep);
    Ok(SnapshotOutcome { path })
}

/// Timestamped, never reused: an interrupted save can only leave its temporary
/// file, and existing snapshots are never overwritten.
fn snapshot_path(dir: &Path, stem: &str) -> PathBuf {
    let (date, time) = stamp(SystemTime::now());
    let mut path = dir.join(format!("{stem}-{date}-{time}.nsproj"));
    let mut suffix = 0;
    while path.exists() {
        suffix += 1;
        path = dir.join(format!("{stem}-{date}-{time}-{suffix}.nsproj"));
    }
    path
}

fn snapshot_files(dir: &Path, stem: &str) -> io::Result<Vec<PathBuf>> {
    let prefix = format!("{stem}-");
    let mut files = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(files),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let is_snapshot = name.starts_with(&prefix)
            && name
                .rsplit_once('.')
                .is_some_and(|(_, extension)| extension.eq_ignore_ascii_case("nsproj"));
        if is_snapshot {
            files.push(path);
        }
    }
    // Names carry a fixed-width timestamp, so lexical order is chronological.
    files.sort();
    Ok(files)
}

/// Remove `*.tmp-*` leftovers from crashed saves. They are only deleted once
/// they are old enough that no running process can still be writing them.
fn remove_stale_temporaries(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let path = entry.path();
        let is_temporary = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.contains(".tmp-"));
        if !is_temporary {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| {
                now.duration_since(modified).unwrap_or_default() > Duration::from_secs(3_600)
            });
        if old {
            let _ = fs::remove_file(&path);
        }
    }
}

fn recovery_root() -> PathBuf {
    if cfg!(windows) {
        if let Ok(base) = std::env::var("LOCALAPPDATA") {
            return PathBuf::from(base).join("n-shade").join("autosave");
        }
    } else {
        if let Ok(base) = std::env::var("XDG_DATA_HOME") {
            return PathBuf::from(base).join("n-shade").join("autosave");
        }
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("n-shade")
                .join("autosave");
        }
    }
    std::env::temp_dir().join("n-shade-autosave")
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

fn settings_path() -> PathBuf {
    if cfg!(windows) {
        if let Ok(base) = std::env::var("LOCALAPPDATA") {
            return PathBuf::from(base).join("n-shade").join("settings.json");
        }
    } else {
        if let Ok(base) = std::env::var("XDG_CONFIG_HOME") {
            return PathBuf::from(base).join("n-shade").join("settings.json");
        }
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home)
                .join(".config")
                .join("n-shade")
                .join("settings.json");
        }
    }
    std::env::temp_dir().join("n-shade-settings.json")
}

/// Apply the persisted autosave settings to a fresh editor state.
pub fn load_settings(state: &mut EditorState) {
    let (enabled, backups) = load_settings_from(&settings_path());
    if let Some(enabled) = enabled {
        state.autosave_enabled = enabled;
    }
    if let Some(backups) = backups {
        state.autosave_backups = backups;
    }
}

/// Persist the autosave settings. A missing or unreadable file simply keeps the
/// defaults; settings corruption must never stop the editor from starting.
pub fn save_settings(enabled: bool, backups: usize) -> io::Result<()> {
    save_settings_to(&settings_path(), enabled, backups)
}

fn load_settings_from(path: &Path) -> (Option<bool>, Option<usize>) {
    let Ok(text) = fs::read_to_string(path) else {
        return (None, None);
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return (None, None);
    };
    let enabled = value.get("autosave_enabled").and_then(Value::as_bool);
    let backups = value
        .get("autosave_backups")
        .and_then(Value::as_u64)
        .map(|backups| (backups as usize).clamp(MIN_BACKUPS, MAX_BACKUPS));
    (enabled, backups)
}

fn save_settings_to(path: &Path, enabled: bool, backups: usize) -> io::Result<()> {
    let body = serde_json::json!({
        "autosave_enabled": enabled,
        "autosave_backups": backups.clamp(MIN_BACKUPS, MAX_BACKUPS),
    });
    let text = serde_json::to_vec_pretty(&body).map_err(io::Error::other)?;
    ns_core::io::write_atomic(path, &text).map_err(io::Error::other)
}

// ---------------------------------------------------------------------------
// Timestamps
// ---------------------------------------------------------------------------

/// `(YYYYMMDD, HHMMSS)` in UTC. Fixed-width strings sort lexicographically, so
/// snapshot names sort chronologically without parsing.
fn stamp(now: SystemTime) -> (String, String) {
    let secs = now
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    let (year, month, day) = civil_from_days((secs / 86_400) as i64);
    let rem = secs % 86_400;
    (
        format!("{year:04}{month:02}{day:02}"),
        format!("{:02}{:02}{:02}", rem / 3_600, (rem / 60) % 60, rem % 60),
    )
}

/// Howard Hinnant's civil-from-days algorithm: UNIX days to `(year, month, day)`
/// in the proleptic Gregorian calendar, without a date-time dependency.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (year + i64::from(month <= 2), month as u32, day as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ns_core::Document;

    fn document() -> Document {
        Document::new_with_base(glam::UVec2::splat(64))
    }

    fn temporary_files(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".tmp-"))
            .collect()
    }

    #[test]
    fn a_snapshot_round_trips_and_leaves_no_temporaries() {
        let dir = tempfile::tempdir().unwrap();
        let mut doc = document();
        doc.layers[0].name = "recovered work".into();

        let outcome = write_snapshot(dir.path(), "demo", 3, &doc).unwrap();

        let loaded = ns_core::io::load_project_ron(&outcome.path).unwrap();
        assert_eq!(loaded.layers[0].name, "recovered work");
        assert!(temporary_files(dir.path()).is_empty());
    }

    #[test]
    fn pruning_keeps_the_newest_snapshots_and_unrelated_files() {
        let dir = tempfile::tempdir().unwrap();
        for day in 1..=5 {
            fs::write(
                dir.path()
                    .join(format!("demo-202601{day:02}-000000.nsproj")),
                b"{}",
            )
            .unwrap();
        }
        fs::write(dir.path().join("notes.txt"), b"keep").unwrap();
        fs::write(dir.path().join("other-20260101-000000.nsproj"), b"keep").unwrap();

        let removed = prune(dir.path(), "demo", 2).unwrap();

        assert_eq!(removed, 3);
        let remaining: Vec<_> = snapshot_files(dir.path(), "demo")
            .unwrap()
            .into_iter()
            .filter_map(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .collect();
        assert_eq!(
            remaining,
            ["demo-20260104-000000.nsproj", "demo-20260105-000000.nsproj"]
        );
        assert!(dir.path().join("notes.txt").is_file());
        assert!(dir.path().join("other-20260101-000000.nsproj").is_file());
    }

    #[test]
    fn the_latest_snapshot_is_the_newest_by_name() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("demo-20260101-000000.nsproj"), b"").unwrap();
        fs::write(dir.path().join("demo-20260103-000000.nsproj"), b"").unwrap();
        fs::write(dir.path().join("demo-20260102-000000.nsproj"), b"").unwrap();

        let latest = latest_snapshot(dir.path(), "demo").unwrap().unwrap();

        assert_eq!(
            latest.file_name().unwrap().to_string_lossy(),
            "demo-20260103-000000.nsproj"
        );
    }

    #[test]
    fn a_missing_autosave_folder_reports_no_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        assert!(latest_snapshot(&dir.path().join("missing"), "demo")
            .unwrap()
            .is_none());
    }

    #[test]
    fn autosave_folders_live_beside_the_project() {
        let path = Path::new("C:/work/demo.nsproj");
        let dir = autosave_dir(Some(path));
        assert_eq!(dir, Path::new("C:/work/demo.autosave"));
        assert_eq!(snapshot_stem(Some(path)), "demo");
        assert_eq!(snapshot_stem(None), "untitled");
    }

    #[test]
    fn settings_round_trip_and_clamp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        save_settings_to(&path, false, 9).unwrap();
        assert_eq!(load_settings_from(&path), (Some(false), Some(9)));

        fs::write(
            &path,
            br#"{"autosave_enabled": true, "autosave_backups": 9999}"#,
        )
        .unwrap();
        assert_eq!(load_settings_from(&path), (Some(true), Some(MAX_BACKUPS)));
    }

    #[test]
    fn corrupt_settings_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, b"not json").unwrap();

        assert_eq!(load_settings_from(&path), (None, None));
    }

    #[test]
    fn snapshot_timestamps_are_fixed_width_and_sortable() {
        let (date_a, time_a) = stamp(UNIX_EPOCH + Duration::from_secs(1_700_000_000));
        let (date_b, time_b) = stamp(UNIX_EPOCH + Duration::from_secs(1_700_000_060));
        assert_eq!(date_a.len(), 8);
        assert_eq!(time_a.len(), 6);
        assert!(format!("{date_a}{time_a}") < format!("{date_b}{time_b}"));
    }

    #[test]
    fn the_civil_calendar_matches_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_000), (2024, 10, 4));
        assert_eq!(civil_from_days(19_722), (2023, 12, 31));
    }
}
