//! Persistence: atomic writes, backups before risky changes, and recovery
//! from unreadable files.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::migrate::MigrationReport;
use super::schema::Config;
use super::{ConfigError, MAX_CONFIG_BYTES, parse_config, to_json};

/// Number of backup files kept next to the configuration.
const MAX_BACKUPS: usize = 10;

/// What happened while loading the configuration.
#[derive(Debug)]
pub enum LoadOutcome {
    /// No configuration file existed; defaults are in use (nothing written yet).
    Fresh(Config),
    /// The file was read successfully, possibly after migrating it.
    Loaded {
        config: Config,
        migration: Option<MigrationReport>,
        /// Copy of the pre-migration file, when a migration ran.
        backup: Option<PathBuf>,
    },
    /// The file could not be used. Defaults are in use and the original file
    /// was copied to `backup` (when possible) before anything overwrites it.
    Recovered {
        config: Config,
        error: String,
        backup: Option<PathBuf>,
    },
}

impl LoadOutcome {
    pub fn config(&self) -> &Config {
        match self {
            LoadOutcome::Fresh(config)
            | LoadOutcome::Loaded { config, .. }
            | LoadOutcome::Recovered { config, .. } => config,
        }
    }
}

/// Reads and writes `config.json` in the per-user configuration directory.
#[derive(Debug)]
pub struct ConfigStore {
    path: PathBuf,
    /// Last content written or read, used to skip redundant writes.
    last_written: Option<String>,
    /// Set when the file on disk was not used (or not migrated) and could
    /// not be backed up during `load`. The next `save` backs it up first and
    /// refuses to overwrite it while that fails.
    pending_backup: Option<String>,
}

/// Attempts to read the configuration before giving up. Antivirus, backup
/// and sync tools can hold the file briefly.
const READ_ATTEMPTS: u32 = 3;

impl ConfigStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            last_written: None,
            pending_backup: None,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&mut self) -> LoadOutcome {
        self.pending_backup = None;
        let metadata = match fs::metadata(&self.path) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return LoadOutcome::Fresh(Config::default());
            }
            Err(e) => {
                let error = io_error(&self.path, e).to_string();
                return self.recovered(error, "invalid");
            }
        };
        if metadata.len() > MAX_CONFIG_BYTES {
            let error = ConfigError::TooLarge {
                size: metadata.len(),
                limit: MAX_CONFIG_BYTES,
            }
            .to_string();
            return self.recovered(error, "invalid");
        }
        let text = match read_with_retries(&self.path) {
            Ok(text) => text,
            Err(e) => {
                let error = io_error(&self.path, e).to_string();
                return self.recovered(error, "invalid");
            }
        };
        match parse_config(&text) {
            Ok((config, report)) if report.migrated() => {
                let label = format!("v{}", report.from_version);
                let backup = self.backup_or_defer(&label);
                // Persist the migrated form only after the backup exists.
                if backup.is_some()
                    && let Err(e) = self.save(&config)
                {
                    tracing::warn!(error = %e, "could not persist migrated configuration");
                }
                LoadOutcome::Loaded {
                    config,
                    migration: Some(report),
                    backup,
                }
            }
            Ok((config, _)) => {
                self.last_written = Some(text);
                LoadOutcome::Loaded {
                    config,
                    migration: None,
                    backup: None,
                }
            }
            Err(error) => self.recovered(error.to_string(), "invalid"),
        }
    }

    fn recovered(&mut self, error: String, label: &str) -> LoadOutcome {
        let backup = self.backup_or_defer(label);
        LoadOutcome::Recovered {
            config: Config::default(),
            error,
            backup,
        }
    }

    /// Backs up the current file. If that fails, the next `save` must retry
    /// the backup before it may overwrite the file.
    fn backup_or_defer(&mut self, label: &str) -> Option<PathBuf> {
        match self.backup_current(label) {
            Ok(backup) => backup,
            Err(e) => {
                tracing::warn!(error = %e, "could not back up the configuration file; it will not be overwritten until a backup succeeds");
                self.pending_backup = Some(label.to_owned());
                None
            }
        }
    }

    /// Writes the configuration atomically. Returns `Ok(false)` when the
    /// content is unchanged and nothing was written.
    ///
    /// If `load` could not back up a file it did not use, the backup is
    /// retried first and the file is never overwritten while that fails.
    pub fn save(&mut self, config: &Config) -> Result<bool, ConfigError> {
        let json = to_json(config)?;
        if self.last_written.as_deref() == Some(json.as_str()) {
            return Ok(false);
        }
        if let Some(label) = self.pending_backup.clone() {
            self.backup_current(&label)
                .map_err(|e| ConfigError::BackupFailed(e.to_string()))?;
            self.pending_backup = None;
        }
        write_atomic(&self.path, json.as_bytes()).map_err(|e| io_error(&self.path, e))?;
        self.last_written = Some(json);
        Ok(true)
    }

    /// Copies the current file to `config.backup-<label>-<unix seconds>.json`.
    /// Returns `Ok(None)` when there is no file to back up. When the newest
    /// existing backup already has the same contents, that backup is returned
    /// instead of creating a duplicate.
    pub fn backup_current(&self, label: &str) -> Result<Option<PathBuf>, ConfigError> {
        match self.path.try_exists() {
            Ok(true) => {}
            Ok(false) => return Ok(None),
            Err(e) => return Err(io_error(&self.path, e)),
        }
        let contents = fs::read(&self.path).map_err(|e| io_error(&self.path, e))?;
        let dir = self.path.parent().unwrap_or_else(|| Path::new("."));
        if let Some(newest) = list_backups(dir).pop()
            && fs::read(&newest).is_ok_and(|existing| existing == contents)
        {
            return Ok(Some(newest));
        }
        let stamp = crate::time::now_unix_ms() / 1000;
        let label: String = label
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        let mut target = dir.join(format!("config.backup-{label}-{stamp}.json"));
        let mut n = 1;
        while target.exists() {
            target = dir.join(format!("config.backup-{label}-{stamp}-{n}.json"));
            n += 1;
        }
        // Written (not copied) so the backup's modification time is the time
        // of the backup; `fs::copy` keeps the source's time on Windows, which
        // would make pruning treat a fresh backup as the oldest.
        write_new(&target, &contents).map_err(|e| io_error(&target, e))?;
        prune_backups(dir, &target);
        Ok(Some(target))
    }
}

fn read_with_retries(path: &Path) -> io::Result<String> {
    let mut attempt = 1;
    loop {
        match fs::read_to_string(path) {
            Ok(text) => return Ok(text),
            // Invalid UTF-8 or a vanished file will not fix itself.
            Err(e)
                if attempt >= READ_ATTEMPTS
                    || matches!(e.kind(), io::ErrorKind::InvalidData | io::ErrorKind::NotFound) =>
            {
                return Err(e);
            }
            Err(_) => {
                std::thread::sleep(Duration::from_millis(100 * u64::from(attempt)));
                attempt += 1;
            }
        }
    }
}

fn write_new(path: &Path, contents: &[u8]) -> io::Result<()> {
    let result = (|| {
        let mut file = fs::OpenOptions::new().write(true).create_new(true).open(path)?;
        file.write_all(contents)?;
        file.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    result
}

/// Reads and validates a configuration file selected for import.
pub fn read_config_file(path: &Path) -> Result<(Config, MigrationReport), ConfigError> {
    let metadata = fs::metadata(path).map_err(|e| io_error(path, e))?;
    if metadata.len() > MAX_CONFIG_BYTES {
        return Err(ConfigError::TooLarge {
            size: metadata.len(),
            limit: MAX_CONFIG_BYTES,
        });
    }
    let text = fs::read_to_string(path).map_err(|e| io_error(path, e))?;
    parse_config(&text)
}

/// Writes `config` as pretty JSON to `path` atomically (used for export).
pub fn write_config_file(path: &Path, config: &Config) -> Result<(), ConfigError> {
    let json = to_json(config)?;
    write_atomic(path, json.as_bytes()).map_err(|e| io_error(path, e))
}

fn io_error(path: &Path, source: io::Error) -> ConfigError {
    ConfigError::Io {
        path: path.display().to_string(),
        source,
    }
}

/// Writes to a temporary file in the same directory, flushes it to disk and
/// renames it over the destination. On Windows, `std::fs::rename` replaces the
/// destination (MoveFileEx with replace semantics). A reader therefore sees
/// either the old or the new file, never a partially written one. Renames are
/// retried briefly because antivirus or indexing software can hold the file.
pub fn write_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir)?;
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "config.json".to_owned());
    let tmp = dir.join(format!(".{file_name}.{:08x}.tmp", fastrand::u32(..)));
    let written = (|| {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(contents)?;
        file.sync_all()
    })();
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    let mut attempt = 0u32;
    loop {
        match fs::rename(&tmp, path) {
            Ok(()) => return Ok(()),
            Err(e) if attempt < 4 && e.kind() == io::ErrorKind::PermissionDenied => {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(25 * u64::from(attempt)));
            }
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                return Err(e);
            }
        }
    }
}

/// Backup files in `dir`, oldest first.
fn list_backups(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut backups: Vec<(std::time::SystemTime, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.starts_with("config.backup-") && name.ends_with(".json")
        })
        .filter_map(|e| {
            let modified = e.metadata().ok()?.modified().ok()?;
            Some((modified, e.path()))
        })
        .collect();
    backups.sort();
    backups.into_iter().map(|(_, path)| path).collect()
}

/// Keeps the newest `MAX_BACKUPS` backups. `keep` (the backup just made) is
/// never removed.
fn prune_backups(dir: &Path, keep: &Path) {
    let backups: Vec<PathBuf> = list_backups(dir)
        .into_iter()
        .filter(|path| path != keep)
        .collect();
    let excess = (backups.len() + 1).saturating_sub(MAX_BACKUPS);
    for path in backups.into_iter().take(excess) {
        let _ = fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CURRENT_SCHEMA_VERSION, Theme};

    fn store_in(dir: &tempfile::TempDir) -> ConfigStore {
        ConfigStore::new(dir.path().join("config.json"))
    }

    fn files(dir: &tempfile::TempDir) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn missing_file_gives_fresh_defaults_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store_in(&dir);
        assert!(matches!(store.load(), LoadOutcome::Fresh(_)));
        assert!(files(&dir).is_empty());
    }

    #[test]
    fn save_then_load_round_trips_and_skips_redundant_writes() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store_in(&dir);
        let mut config = Config::default();
        config.ui.theme = Theme::Dark;
        config.discord.application_id = Some("123456789012345678".into());
        assert!(store.save(&config).unwrap());
        assert!(
            !store.save(&config).unwrap(),
            "unchanged config must not be rewritten"
        );

        let mut reloaded = store_in(&dir);
        match reloaded.load() {
            LoadOutcome::Loaded {
                config: loaded,
                migration: None,
                ..
            } => {
                assert_eq!(loaded, config)
            }
            other => panic!("unexpected outcome {other:?}"),
        }
        assert!(!reloaded.save(&config).unwrap());
        // Only the config file remains: no temp files are left behind.
        assert_eq!(files(&dir), vec!["config.json".to_owned()]);
    }

    #[test]
    fn partially_written_file_is_backed_up_and_defaults_used() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            "{\"schema_version\": 1, \"presence\": {",
        )
        .unwrap();
        let mut store = store_in(&dir);
        match store.load() {
            LoadOutcome::Recovered {
                config,
                error,
                backup,
            } => {
                assert_eq!(config, Config::default());
                assert!(error.contains("not valid JSON"));
                let backup = backup.expect("backup created");
                assert!(backup.exists());
            }
            other => panic!("unexpected outcome {other:?}"),
        }
        // The broken original is still in place until the next save.
        assert!(dir.path().join("config.json").exists());
    }

    #[test]
    fn invalid_values_are_reported_not_silently_fixed() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"schema_version":1,"presence":{"details_template":"{oops}"}}"#,
        )
        .unwrap();
        let mut store = store_in(&dir);
        match store.load() {
            LoadOutcome::Recovered { error, .. } => assert!(error.contains("{oops}")),
            other => panic!("unexpected outcome {other:?}"),
        }
    }

    #[test]
    fn migration_creates_backup_then_persists_new_schema() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"client_id":"123456789012345678","exclusions":["a.exe"]}"#,
        )
        .unwrap();
        let mut store = store_in(&dir);
        match store.load() {
            LoadOutcome::Loaded {
                config,
                migration: Some(report),
                backup: Some(backup),
            } => {
                assert_eq!(report.from_version, 0);
                assert_eq!(config.exclusions.len(), 1);
                let original = fs::read_to_string(backup).unwrap();
                assert!(original.contains("client_id"));
            }
            other => panic!("unexpected outcome {other:?}"),
        }
        let written = fs::read_to_string(dir.path().join("config.json")).unwrap();
        let value: serde_json::Value = serde_json::from_str(&written).unwrap();
        assert_eq!(value["schema_version"], CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn newer_schema_is_preserved_via_backup() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.json"), r#"{"schema_version": 7}"#).unwrap();
        let mut store = store_in(&dir);
        match store.load() {
            LoadOutcome::Recovered {
                error,
                backup: Some(_),
                ..
            } => {
                assert!(error.contains("newer version"))
            }
            other => panic!("unexpected outcome {other:?}"),
        }
    }

    #[test]
    fn backups_are_pruned() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(&dir);
        for i in 0..(MAX_BACKUPS + 3) {
            fs::write(store.path(), format!("{{\"n\": {i}}}")).unwrap();
            store.backup_current("test").unwrap();
        }
        let count = files(&dir)
            .iter()
            .filter(|n| n.starts_with("config.backup-"))
            .count();
        assert_eq!(count, MAX_BACKUPS);
    }

    fn set_modified(path: &Path, age_secs: u64) {
        let file = fs::OpenOptions::new().write(true).open(path).unwrap();
        let time = std::time::SystemTime::now() - Duration::from_secs(age_secs);
        file.set_modified(time).unwrap();
    }

    #[test]
    fn a_new_backup_survives_pruning_even_if_the_source_is_old() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(&dir);
        for i in 0..MAX_BACKUPS {
            let path = dir.path().join(format!("config.backup-old-{i}.json"));
            fs::write(&path, format!("old {i}")).unwrap();
            set_modified(&path, 3600 + i as u64);
        }
        fs::write(store.path(), "restored from an old disk").unwrap();
        set_modified(store.path(), 10 * 365 * 24 * 3600);

        let backup = store.backup_current("invalid").unwrap().unwrap();
        assert_eq!(fs::read_to_string(&backup).unwrap(), "restored from an old disk");
        let count = files(&dir)
            .iter()
            .filter(|n| n.starts_with("config.backup-"))
            .count();
        assert_eq!(count, MAX_BACKUPS);
        // The oldest previous backup made room.
        assert!(
            !dir.path()
                .join(format!("config.backup-old-{}.json", MAX_BACKUPS - 1))
                .exists()
        );
    }

    #[test]
    fn repeated_backups_of_the_same_file_are_not_duplicated() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.json"), "{ broken").unwrap();
        for _ in 0..5 {
            let mut store = store_in(&dir);
            assert!(matches!(
                store.load(),
                LoadOutcome::Recovered { backup: Some(_), .. }
            ));
        }
        let count = files(&dir)
            .iter()
            .filter(|n| n.starts_with("config.backup-"))
            .count();
        assert_eq!(count, 1);
    }

    #[test]
    fn a_file_that_could_not_be_backed_up_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        // A directory in place of the file: it exists but cannot be read or
        // copied, even with administrator/root rights.
        fs::create_dir(&path).unwrap();
        let mut store = store_in(&dir);
        assert!(matches!(
            store.load(),
            LoadOutcome::Recovered { backup: None, .. }
        ));
        assert!(matches!(
            store.save(&Config::default()),
            Err(ConfigError::BackupFailed(_))
        ));
        assert!(path.is_dir(), "nothing may replace an unpreserved file");

        // Once the original can be read, the save backs it up first.
        fs::remove_dir(&path).unwrap();
        fs::write(&path, r#"{"schema_version": 7}"#).unwrap();
        assert!(store.save(&Config::default()).unwrap());
        let backups: Vec<String> = files(&dir)
            .into_iter()
            .filter(|n| n.starts_with("config.backup-invalid-"))
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(
            fs::read_to_string(dir.path().join(&backups[0])).unwrap(),
            r#"{"schema_version": 7}"#
        );
        // The flag is cleared: later saves don't back up again.
        let mut changed = Config::default();
        changed.ui.theme = Theme::Dark;
        assert!(store.save(&changed).unwrap());
        assert_eq!(
            files(&dir)
                .iter()
                .filter(|n| n.starts_with("config.backup-"))
                .count(),
            1
        );
    }

    #[test]
    fn failed_writes_leave_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        // The destination is a non-empty directory, so the rename fails.
        let target = dir.path().join("config.json");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("inside"), "x").unwrap();
        assert!(write_atomic(&target, b"{}").is_err());
        assert_eq!(files(&dir), vec!["config.json".to_owned()]);
    }

    #[test]
    fn a_utf8_bom_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            "\u{feff}{\"schema_version\": 1, \"ui\": {\"theme\": \"dark\"}}",
        )
        .unwrap();
        let mut store = store_in(&dir);
        match store.load() {
            LoadOutcome::Loaded { config, .. } => assert_eq!(config.ui.theme, Theme::Dark),
            other => panic!("unexpected outcome {other:?}"),
        }
    }

    #[test]
    fn import_rejects_malformed_and_oversized_files() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("bad.json");
        fs::write(&bad, "not json").unwrap();
        assert!(matches!(read_config_file(&bad), Err(ConfigError::Json(_))));

        let big = dir.path().join("big.json");
        fs::write(&big, vec![b' '; (MAX_CONFIG_BYTES + 1) as usize]).unwrap();
        assert!(matches!(
            read_config_file(&big),
            Err(ConfigError::TooLarge { .. })
        ));

        let good = dir.path().join("good.json");
        write_config_file(&good, &Config::default()).unwrap();
        assert!(read_config_file(&good).is_ok());
    }
}
