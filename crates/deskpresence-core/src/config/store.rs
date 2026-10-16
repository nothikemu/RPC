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
}

impl ConfigStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            last_written: None,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&mut self) -> LoadOutcome {
        let metadata = match fs::metadata(&self.path) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return LoadOutcome::Fresh(Config::default());
            }
            Err(e) => {
                return LoadOutcome::Recovered {
                    config: Config::default(),
                    error: io_error(&self.path, e).to_string(),
                    backup: None,
                };
            }
        };
        if metadata.len() > MAX_CONFIG_BYTES {
            let backup = self.backup_current("invalid").ok().flatten();
            return LoadOutcome::Recovered {
                config: Config::default(),
                error: ConfigError::TooLarge {
                    size: metadata.len(),
                    limit: MAX_CONFIG_BYTES,
                }
                .to_string(),
                backup,
            };
        }
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(e) => {
                let backup = self.backup_current("invalid").ok().flatten();
                return LoadOutcome::Recovered {
                    config: Config::default(),
                    error: io_error(&self.path, e).to_string(),
                    backup,
                };
            }
        };
        match parse_config(&text) {
            Ok((config, report)) if report.migrated() => {
                let label = format!("v{}", report.from_version);
                let backup = self.backup_current(&label).ok().flatten();
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
            Err(error) => {
                let backup = self.backup_current("invalid").ok().flatten();
                LoadOutcome::Recovered {
                    config: Config::default(),
                    error: error.to_string(),
                    backup,
                }
            }
        }
    }

    /// Writes the configuration atomically. Returns `Ok(false)` when the
    /// content is unchanged and nothing was written.
    pub fn save(&mut self, config: &Config) -> Result<bool, ConfigError> {
        let json = to_json(config)?;
        if self.last_written.as_deref() == Some(json.as_str()) {
            return Ok(false);
        }
        write_atomic(&self.path, json.as_bytes()).map_err(|e| io_error(&self.path, e))?;
        self.last_written = Some(json);
        Ok(true)
    }

    /// Copies the current file to `config.backup-<label>-<unix seconds>.json`.
    /// Returns `Ok(None)` when there is no file to back up.
    pub fn backup_current(&self, label: &str) -> Result<Option<PathBuf>, ConfigError> {
        if !self.path.exists() {
            return Ok(None);
        }
        let dir = self.path.parent().unwrap_or_else(|| Path::new("."));
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
        fs::copy(&self.path, &target).map_err(|e| io_error(&target, e))?;
        prune_backups(dir);
        Ok(Some(target))
    }
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
    {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(contents)?;
        file.sync_all()?;
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

fn prune_backups(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut backups: Vec<(std::time::SystemTime, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with("config.backup-"))
        .filter_map(|e| {
            let modified = e.metadata().ok()?.modified().ok()?;
            Some((modified, e.path()))
        })
        .collect();
    if backups.len() <= MAX_BACKUPS {
        return;
    }
    backups.sort();
    let excess = backups.len() - MAX_BACKUPS;
    for (_, path) in backups.into_iter().take(excess) {
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
        fs::write(store.path(), "{}").unwrap();
        for _ in 0..(MAX_BACKUPS + 3) {
            store.backup_current("test").unwrap();
        }
        let count = files(&dir)
            .iter()
            .filter(|n| n.starts_with("config.backup-"))
            .count();
        assert_eq!(count, MAX_BACKUPS);
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
