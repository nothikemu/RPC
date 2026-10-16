//! Executable icon extraction and caching.
//!
//! Icons are extracted once per executable *version* (path + file size +
//! modification time), converted to validated PNG and cached on disk under
//! the per-user cache directory. Failures are cached in memory so a broken
//! executable is not retried on every request. Extraction runs on a
//! dedicated thread ([`IconService`]); it is never part of the polling loop.
//!
//! These icons are only used inside DeskPresence's own UI. They are never
//! uploaded anywhere and are not Discord Rich Presence artwork.

#[cfg(windows)]
mod windows;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, SyncSender};
use std::time::Duration;

use base64::Engine;
use tokio::sync::oneshot;

use crate::app_detection::FileStamp;
use crate::app_identity::AppKey;
use crate::config::write_atomic;

/// Edge length of extracted icons in pixels (crisp at 32 px and 150 % scaling).
pub const ICON_SIZE: u32 = 48;
/// Maximum accepted PNG size; anything larger is treated as corrupt.
pub const MAX_ICON_BYTES: usize = 256 * 1024;
/// Bump when the extraction or encoding changes to invalidate old cache files.
const CACHE_FORMAT_VERSION: u32 = 1;
/// Disk cache bound; the oldest files are removed beyond this.
pub const MAX_CACHE_FILES: usize = 400;
/// In-memory entries kept before the map is reset.
const MAX_MEMORY_ENTRIES: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IconError {
    #[error("icon extraction is not supported on this platform")]
    Unsupported,
    #[error("the executable has no usable icon")]
    NoIcon,
    #[error("invalid icon image: {0}")]
    InvalidImage(String),
    #[error("{0}")]
    Os(String),
}

/// Raw RGBA pixels (row-major, top-down, non-premultiplied).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Platform icon extraction.
pub trait IconExtractor {
    fn extract(&mut self, exe_path: &str, size: u32) -> Result<RgbaImage, IconError>;
}

/// Used where no platform extractor exists; the UI shows its fallback icon.
pub struct NoIconExtractor;

impl IconExtractor for NoIconExtractor {
    fn extract(&mut self, _: &str, _: u32) -> Result<RgbaImage, IconError> {
        Err(IconError::Unsupported)
    }
}

/// Creates the platform extractor. Must be called on the thread that will
/// use it (the Windows extractor initializes COM for that thread).
pub fn platform_extractor() -> Box<dyn IconExtractor> {
    #[cfg(windows)]
    {
        Box::new(windows::Win32IconExtractor::new())
    }
    #[cfg(not(windows))]
    {
        Box::new(NoIconExtractor)
    }
}

/// 64-bit FNV-1a: tiny, dependency-free and stable across builds and
/// platforms (unlike `std`'s hasher), which matters for on-disk keys.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Cache key for an executable version at a given icon size.
pub fn cache_key(app_key: &AppKey, stamp: Option<FileStamp>, size: u32) -> String {
    let (len, modified) = stamp.map_or((0, 0), |s| (s.size, s.modified_ms));
    let material = format!(
        "v{CACHE_FORMAT_VERSION}|{}|{len}|{modified}|{size}",
        app_key.as_str()
    );
    format!("{:016x}", fnv1a64(material.as_bytes()))
}

/// Validates dimensions and buffer length, then encodes PNG.
pub fn encode_png(image: &RgbaImage) -> Result<Vec<u8>, IconError> {
    if image.width == 0 || image.height == 0 || image.width > 256 || image.height > 256 {
        return Err(IconError::InvalidImage(format!(
            "unexpected size {}x{}",
            image.width, image.height
        )));
    }
    let expected = (image.width as usize) * (image.height as usize) * 4;
    if image.pixels.len() != expected {
        return Err(IconError::InvalidImage(format!(
            "pixel buffer is {} bytes, expected {expected}",
            image.pixels.len()
        )));
    }
    if image.pixels.chunks_exact(4).all(|p| p[3] == 0) {
        return Err(IconError::NoIcon);
    }
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, image.width, image.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|e| IconError::InvalidImage(e.to_string()))?;
        writer
            .write_image_data(&image.pixels)
            .map_err(|e| IconError::InvalidImage(e.to_string()))?;
    }
    Ok(out)
}

/// Checks that bytes are a small, decodable PNG. Returns its dimensions.
pub fn validate_png(bytes: &[u8]) -> Result<(u32, u32), IconError> {
    if bytes.len() > MAX_ICON_BYTES {
        return Err(IconError::InvalidImage("file too large".into()));
    }
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let reader = decoder
        .read_info()
        .map_err(|e| IconError::InvalidImage(e.to_string()))?;
    let info = reader.info();
    if info.width == 0 || info.height == 0 || info.width > 256 || info.height > 256 {
        return Err(IconError::InvalidImage("unexpected dimensions".into()));
    }
    Ok((info.width, info.height))
}

pub fn png_data_url(bytes: &[u8]) -> String {
    format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

/// Memory + disk icon cache. Not thread-safe; owned by [`IconService`].
pub struct IconCache {
    dir: PathBuf,
    extractor: Box<dyn IconExtractor>,
    /// `None` records a failed extraction (negative cache).
    memory: HashMap<String, Option<Arc<str>>>,
    /// Paths whose file-system access was very slow (e.g. a disconnected
    /// network share), skipped until the recorded time.
    unreachable: HashMap<String, std::time::Instant>,
    extractions: u32,
}

/// File-system or shell access slower than this marks a path as unreachable.
const SLOW_ACCESS: Duration = Duration::from_secs(2);
/// How long an unreachable path is skipped.
const UNREACHABLE_FOR: Duration = Duration::from_secs(600);

impl IconCache {
    pub fn new(dir: impl Into<PathBuf>, extractor: Box<dyn IconExtractor>) -> Self {
        Self {
            dir: dir.into(),
            extractor,
            memory: HashMap::new(),
            unreachable: HashMap::new(),
            extractions: 0,
        }
    }

    /// Number of extraction attempts (for tests and diagnostics).
    pub fn extractions(&self) -> u32 {
        self.extractions
    }

    fn mark_if_slow(&mut self, exe_path: &str, started: std::time::Instant) {
        if started.elapsed() >= SLOW_ACCESS {
            tracing::debug!("icon source is slow to access; skipping it for a while");
            self.unreachable.insert(
                exe_path.to_lowercase(),
                std::time::Instant::now() + UNREACHABLE_FOR,
            );
        }
    }

    /// Returns the icon as a PNG data URL, or `None` to use the fallback icon.
    pub fn get(&mut self, app_key: &AppKey, exe_path: &str) -> Option<Arc<str>> {
        let path_key = exe_path.to_lowercase();
        if let Some(until) = self.unreachable.get(&path_key) {
            if std::time::Instant::now() < *until {
                return None;
            }
            self.unreachable.remove(&path_key);
        }
        let started = std::time::Instant::now();
        let stamp = FileStamp::of(exe_path);
        self.mark_if_slow(exe_path, started);
        let key = cache_key(app_key, stamp, ICON_SIZE);
        if let Some(entry) = self.memory.get(&key) {
            return entry.clone();
        }
        if self.memory.len() >= MAX_MEMORY_ENTRIES {
            self.memory.clear();
        }

        let file = self.dir.join(format!("{key}.png"));
        if let Ok(bytes) = fs::read(&file) {
            if validate_png(&bytes).is_ok() {
                let url: Arc<str> = png_data_url(&bytes).into();
                self.memory.insert(key, Some(url.clone()));
                return Some(url);
            }
            tracing::debug!(file = %file.display(), "discarding corrupt cached icon");
            let _ = fs::remove_file(&file);
        }

        // A missing executable (moved, uninstalled, inaccessible) has no icon.
        if stamp.is_none() {
            self.memory.insert(key, None);
            return None;
        }

        self.extractions += 1;
        let started = std::time::Instant::now();
        let result = self
            .extractor
            .extract(exe_path, ICON_SIZE)
            .and_then(|image| encode_png(&image))
            .and_then(|png| validate_png(&png).map(|_| png));
        self.mark_if_slow(exe_path, started);
        match result {
            Ok(png) => {
                if let Err(error) = write_atomic(&file, &png) {
                    tracing::debug!(%error, "could not write icon cache file");
                }
                let url: Arc<str> = png_data_url(&png).into();
                self.memory.insert(key, Some(url.clone()));
                Some(url)
            }
            Err(error) => {
                tracing::debug!(exe = %app_key.exe_name(), %error, "icon extraction failed");
                self.memory.insert(key, None);
                None
            }
        }
    }

    /// Removes the oldest cache files beyond [`MAX_CACHE_FILES`] and any
    /// stray temporary files.
    pub fn prune(&self) {
        prune_dir(&self.dir, MAX_CACHE_FILES);
    }
}

fn prune_dir(dir: &Path, keep: usize) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".tmp") {
            let _ = fs::remove_file(&path);
            continue;
        }
        if !name.ends_with(".png") {
            continue;
        }
        if let Ok(modified) = entry.metadata().and_then(|m| m.modified()) {
            files.push((modified, path));
        }
    }
    if files.len() <= keep {
        return;
    }
    files.sort();
    let excess = files.len() - keep;
    for (_, path) in files.into_iter().take(excess) {
        let _ = fs::remove_file(path);
    }
}

type IconReply = oneshot::Sender<Vec<(AppKey, Option<Arc<str>>)>>;

struct IconRequest {
    items: Vec<(AppKey, String)>,
    /// Items not started before this point are left unanswered.
    deadline: std::time::Instant,
    reply: IconReply,
}

/// Runs extraction on its own thread with a bounded request queue.
#[derive(Clone)]
pub struct IconService {
    requests: SyncSender<IconRequest>,
}

impl IconService {
    /// Starts the service. `make_extractor` runs on the service thread.
    pub fn spawn<F>(dir: PathBuf, make_extractor: F) -> std::io::Result<Self>
    where
        F: FnOnce() -> Box<dyn IconExtractor> + Send + 'static,
    {
        let (requests, rx) = mpsc::sync_channel::<IconRequest>(32);
        std::thread::Builder::new()
            .name("deskpresence-icons".into())
            .spawn(move || {
                let mut cache = IconCache::new(dir, make_extractor());
                cache.prune();
                while let Ok(request) = rx.recv() {
                    let mut results = Vec::with_capacity(request.items.len());
                    for (key, path) in request.items {
                        if std::time::Instant::now() >= request.deadline {
                            break;
                        }
                        let url = cache.get(&key, &path);
                        results.push((key, url));
                    }
                    let _ = request.reply.send(results);
                }
            })?;
        Ok(Self { requests })
    }

    /// Resolves icons for `(key, exe path)` pairs, waiting at most `limit`.
    ///
    /// Only answered keys are returned: `None` means "this executable has no
    /// usable icon", while a missing key means "not resolved yet" (queue full,
    /// time limit reached), so the caller can ask again later.
    pub async fn get(
        &self,
        items: Vec<(AppKey, String)>,
        limit: Duration,
    ) -> Vec<(AppKey, Option<Arc<str>>)> {
        let (reply, rx) = oneshot::channel();
        let deadline = std::time::Instant::now() + limit.saturating_sub(Duration::from_millis(250));
        if self
            .requests
            .try_send(IconRequest {
                items,
                deadline,
                reply,
            })
            .is_err()
        {
            return Vec::new();
        }
        match tokio::time::timeout(limit, rx).await {
            Ok(Ok(results)) => results,
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeExtractor {
        result: Result<RgbaImage, IconError>,
    }

    impl IconExtractor for FakeExtractor {
        fn extract(&mut self, _: &str, _: u32) -> Result<RgbaImage, IconError> {
            self.result.clone()
        }
    }

    fn solid(size: u32) -> RgbaImage {
        RgbaImage {
            width: size,
            height: size,
            pixels: [10, 20, 30, 255].repeat((size * size) as usize),
        }
    }

    fn key(path: &str) -> AppKey {
        AppKey::from_path(path).unwrap()
    }

    #[test]
    fn cache_keys_are_stable_and_sensitive_to_version() {
        let k = key(r"C:\Apps\a.exe");
        let stamp = Some(FileStamp {
            size: 10,
            modified_ms: 20,
        });
        let a = cache_key(&k, stamp, 48);
        assert_eq!(
            a,
            cache_key(&key(r"c:/apps/A.EXE"), stamp, 48),
            "same identity, same key"
        );
        assert_eq!(a.len(), 16);
        assert_ne!(
            a,
            cache_key(
                &k,
                Some(FileStamp {
                    size: 11,
                    modified_ms: 20
                }),
                48
            )
        );
        assert_ne!(
            a,
            cache_key(
                &k,
                Some(FileStamp {
                    size: 10,
                    modified_ms: 21
                }),
                48
            )
        );
        assert_ne!(a, cache_key(&k, stamp, 32));
        assert_ne!(a, cache_key(&key(r"C:\Apps\b.exe"), stamp, 48));
        // Known FNV-1a test vector.
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
    }

    #[test]
    fn png_round_trip_and_validation() {
        let png = encode_png(&solid(48)).unwrap();
        assert_eq!(validate_png(&png).unwrap(), (48, 48));
        assert!(png_data_url(&png).starts_with("data:image/png;base64,iVBOR"));
        assert!(validate_png(b"not a png").is_err());
        assert!(
            encode_png(&RgbaImage {
                width: 0,
                height: 0,
                pixels: vec![]
            })
            .is_err()
        );
        assert!(
            encode_png(&RgbaImage {
                width: 2,
                height: 2,
                pixels: vec![0; 3]
            })
            .is_err()
        );
        assert!(
            encode_png(&RgbaImage {
                width: 512,
                height: 512,
                pixels: vec![0; 512 * 512 * 4]
            })
            .is_err()
        );
        let transparent = RgbaImage {
            width: 2,
            height: 2,
            pixels: vec![0; 16],
        };
        assert_eq!(encode_png(&transparent), Err(IconError::NoIcon));
    }

    fn fake_exe(dir: &tempfile::TempDir, name: &str) -> String {
        let path = dir.path().join(name);
        fs::write(&path, b"MZ").unwrap();
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn extracts_once_then_serves_from_memory_and_disk() {
        let exes = tempfile::tempdir().unwrap();
        let cache_dir = tempfile::tempdir().unwrap();
        let exe = fake_exe(&exes, "app.exe");
        let k = key(&exe);

        let mut cache = IconCache::new(
            cache_dir.path(),
            Box::new(FakeExtractor {
                result: Ok(solid(48)),
            }),
        );
        let first = cache.get(&k, &exe).unwrap();
        let second = cache.get(&k, &exe).unwrap();
        assert_eq!(first, second);
        assert_eq!(cache.extractions(), 1);

        // A new cache instance reuses the disk file without extracting.
        let mut reloaded = IconCache::new(
            cache_dir.path(),
            Box::new(FakeExtractor {
                result: Err(IconError::NoIcon),
            }),
        );
        assert_eq!(reloaded.get(&k, &exe), Some(first));
        assert_eq!(reloaded.extractions(), 0);
    }

    #[test]
    fn failures_are_negatively_cached_and_missing_files_skip_extraction() {
        let exes = tempfile::tempdir().unwrap();
        let cache_dir = tempfile::tempdir().unwrap();
        let exe = fake_exe(&exes, "noicon.exe");
        let mut cache = IconCache::new(
            cache_dir.path(),
            Box::new(FakeExtractor {
                result: Err(IconError::NoIcon),
            }),
        );
        assert_eq!(cache.get(&key(&exe), &exe), None);
        assert_eq!(cache.get(&key(&exe), &exe), None);
        assert_eq!(cache.extractions(), 1);

        let missing = exes.path().join("gone.exe").to_string_lossy().into_owned();
        assert_eq!(cache.get(&key(&missing), &missing), None);
        assert_eq!(cache.extractions(), 1);
    }

    #[test]
    fn corrupt_cache_files_are_replaced() {
        let exes = tempfile::tempdir().unwrap();
        let cache_dir = tempfile::tempdir().unwrap();
        let exe = fake_exe(&exes, "app.exe");
        let k = key(&exe);
        let file_key = cache_key(&k, FileStamp::of(&exe), ICON_SIZE);
        fs::write(cache_dir.path().join(format!("{file_key}.png")), b"garbage").unwrap();
        let mut cache = IconCache::new(
            cache_dir.path(),
            Box::new(FakeExtractor {
                result: Ok(solid(16)),
            }),
        );
        assert!(cache.get(&k, &exe).is_some());
        assert_eq!(cache.extractions(), 1);
        let bytes = fs::read(cache_dir.path().join(format!("{file_key}.png"))).unwrap();
        assert!(validate_png(&bytes).is_ok());
    }

    #[test]
    fn prune_bounds_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..12 {
            fs::write(dir.path().join(format!("{i:02}.png")), b"x").unwrap();
        }
        fs::write(dir.path().join(".x.tmp"), b"x").unwrap();
        prune_dir(dir.path(), 5);
        let remaining = fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(remaining, 5);
    }

    struct SlowExtractor;

    impl IconExtractor for SlowExtractor {
        fn extract(&mut self, _: &str, _: u32) -> Result<RgbaImage, IconError> {
            std::thread::sleep(SLOW_ACCESS + Duration::from_millis(50));
            Err(IconError::NoIcon)
        }
    }

    #[test]
    fn slow_sources_are_skipped_for_a_while() {
        let exes = tempfile::tempdir().unwrap();
        let cache_dir = tempfile::tempdir().unwrap();
        let exe = fake_exe(&exes, "slow.exe");
        let mut cache = IconCache::new(cache_dir.path(), Box::new(SlowExtractor));
        assert_eq!(cache.get(&key(&exe), &exe), None);
        // Second call returns immediately without touching the slow source.
        let started = std::time::Instant::now();
        assert_eq!(cache.get(&key(&exe), &exe), None);
        assert!(started.elapsed() < Duration::from_millis(500));
        assert_eq!(cache.extractions(), 1);
    }

    #[tokio::test]
    async fn unanswered_keys_are_omitted_not_reported_as_missing_icons() {
        let exes = tempfile::tempdir().unwrap();
        let cache_dir = tempfile::tempdir().unwrap();
        let slow = fake_exe(&exes, "slow.exe");
        let fast = fake_exe(&exes, "fast.exe");
        let service = IconService::spawn(cache_dir.path().to_path_buf(), || Box::new(SlowExtractor)).unwrap();
        let results = service
            .get(
                vec![(key(&slow), slow.clone()), (key(&fast), fast.clone())],
                Duration::from_millis(600),
            )
            .await;
        // The slow item exceeded the limit: nothing is claimed about either key.
        assert!(results.iter().all(|(k, _)| *k != key(&fast)), "{results:?}");
    }

    #[tokio::test]
    async fn service_answers_requests() {
        let exes = tempfile::tempdir().unwrap();
        let cache_dir = tempfile::tempdir().unwrap();
        let exe = fake_exe(&exes, "svc.exe");
        let service = IconService::spawn(cache_dir.path().to_path_buf(), || {
            Box::new(FakeExtractor {
                result: Ok(solid(48)),
            })
        })
        .unwrap();
        let results = service
            .get(vec![(key(&exe), exe.clone())], Duration::from_secs(5))
            .await;
        assert_eq!(results.len(), 1);
        assert!(results[0].1.is_some());
    }
}
