//! File logging with size-based rotation and a runtime-adjustable level.
//!
//! Logs go to `<log dir>/deskpresence.log`, rotated at 2 MB and keeping three
//! older files, so logs never use more than about 8 MB. Window titles and
//! Discord user names are never logged.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use deskpresence_core::config::LogLevel;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{Registry, reload};

pub const LOG_FILE_NAME: &str = "deskpresence.log";
pub const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;
pub const KEEP_ROTATED: u32 = 3;

/// A file writer that rotates when the current file would exceed `max_bytes`.
pub struct RotatingFile {
    path: PathBuf,
    file: Option<File>,
    size: u64,
    max_bytes: u64,
    keep: u32,
}

impl RotatingFile {
    pub fn open(path: PathBuf, max_bytes: u64, keep: u32) -> io::Result<Self> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let size = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Self {
            path,
            file: Some(file),
            size,
            max_bytes,
            keep,
        })
    }

    fn rotated(&self, index: u32) -> PathBuf {
        let mut name = self.path.as_os_str().to_owned();
        name.push(format!(".{index}"));
        PathBuf::from(name)
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.file = None;
        if self.keep == 0 {
            let _ = fs::remove_file(&self.path);
        } else {
            let _ = fs::remove_file(self.rotated(self.keep));
            for index in (1..self.keep).rev() {
                let from = self.rotated(index);
                if from.exists() {
                    let _ = fs::rename(&from, self.rotated(index + 1));
                }
            }
            let _ = fs::rename(&self.path, self.rotated(1));
        }
        self.file = Some(OpenOptions::new().create(true).append(true).open(&self.path)?);
        self.size = 0;
        Ok(())
    }

    fn write_bytes(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.size > 0 && self.size + buf.len() as u64 > self.max_bytes {
            self.rotate()?;
        }
        let file = match self.file.as_mut() {
            Some(file) => file,
            None => {
                self.file = Some(OpenOptions::new().create(true).append(true).open(&self.path)?);
                self.file
                    .as_mut()
                    .ok_or_else(|| io::Error::other("log file unavailable"))?
            }
        };
        file.write_all(buf)?;
        self.size += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush_file(&mut self) -> io::Result<()> {
        match self.file.as_mut() {
            Some(file) => file.flush(),
            None => Ok(()),
        }
    }
}

/// Cloneable handle used as the `tracing` writer.
#[derive(Clone)]
pub struct SharedLog(Arc<Mutex<RotatingFile>>);

impl SharedLog {
    fn lock(&self) -> MutexGuard<'_, RotatingFile> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Write for SharedLog {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.lock().write_bytes(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.lock().flush_file()
    }
}

impl<'a> MakeWriter<'a> for SharedLog {
    type Writer = SharedLog;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Controls the active log level and flushes on exit.
pub struct LogControl {
    reload: Option<reload::Handle<LevelFilter, Registry>>,
    file: Option<SharedLog>,
    path: PathBuf,
}

impl LogControl {
    pub fn set_level(&self, level: LogLevel) {
        if let Some(handle) = &self.reload {
            let filter = level_filter(level);
            if handle.modify(|current| *current = filter).is_ok() {
                tracing::info!(?level, "log level changed");
            }
        }
    }

    pub fn flush(&self) {
        if let Some(file) = &self.file {
            let _ = file.clone().flush();
        }
    }

    pub fn log_path(&self) -> &Path {
        &self.path
    }
}

pub fn level_filter(level: LogLevel) -> LevelFilter {
    match level {
        LogLevel::Error => LevelFilter::ERROR,
        LogLevel::Warn => LevelFilter::WARN,
        LogLevel::Info => LevelFilter::INFO,
        LogLevel::Debug => LevelFilter::DEBUG,
        LogLevel::Trace => LevelFilter::TRACE,
    }
}

/// Installs the global subscriber. Falls back to stderr-only logging if the
/// log file cannot be opened; logging problems never prevent startup.
pub fn init(log_dir: &Path, level: LogLevel) -> LogControl {
    let path = log_dir.join(LOG_FILE_NAME);
    let (filter, reload_handle) = reload::Layer::new(level_filter(level));
    let file = RotatingFile::open(path.clone(), MAX_LOG_BYTES, KEEP_ROTATED)
        .map(|f| SharedLog(Arc::new(Mutex::new(f))))
        .ok();

    let file_layer = file.clone().map(|writer| {
        tracing_subscriber::fmt::layer()
            .with_writer(writer)
            .with_ansi(false)
            .with_target(false)
    });
    let stderr_layer = cfg!(debug_assertions).then(|| {
        tracing_subscriber::fmt::layer()
            .with_writer(io::stderr)
            .with_target(false)
    });
    let installed = tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(stderr_layer)
        .try_init()
        .is_ok();

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("panic: {info}");
        default_hook(info);
    }));

    LogControl {
        reload: installed.then_some(reload_handle),
        file,
        path,
    }
}

/// The last `max_bytes` of the current log file (for diagnostics exports).
pub fn read_tail(path: &Path, max_bytes: u64) -> String {
    let Ok(mut file) = File::open(path) else {
        return String::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(max_bytes);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut bytes = Vec::new();
    let _ = file.take(max_bytes).read_to_end(&mut bytes);
    let text = String::from_utf8_lossy(&bytes).into_owned();
    // Drop a partial first line.
    match (start > 0, text.find('\n')) {
        (true, Some(i)) => text[i + 1..].to_owned(),
        _ => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotates_and_bounds_disk_usage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.log");
        let mut log = RotatingFile::open(path.clone(), 100, 2).unwrap();
        for i in 0..50 {
            log.write_bytes(format!("line {i:03} ........\n").as_bytes())
                .unwrap();
        }
        let mut names: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, vec!["test.log", "test.log.1", "test.log.2"]);
        for name in &names {
            assert!(fs::metadata(dir.path().join(name)).unwrap().len() <= 100);
        }
        let current = fs::read_to_string(&path).unwrap();
        assert!(current.contains("line 049"));
    }

    #[test]
    fn tail_returns_whole_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.log");
        fs::write(&path, "first line\nsecond line\nthird line\n").unwrap();
        assert_eq!(read_tail(&path, 1000), "first line\nsecond line\nthird line\n");
        assert_eq!(read_tail(&path, 15), "third line\n");
        assert_eq!(read_tail(&dir.path().join("missing.log"), 10), "");
    }
}
