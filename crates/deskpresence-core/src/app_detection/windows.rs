//! Win32 implementation of [`WindowSystem`].
//!
//! Window filtering mirrors what the taskbar/Alt+Tab treat as application
//! windows: visible, not cloaked (DWM), unowned or `WS_EX_APPWINDOW`, not a
//! tool window, and not a shell surface (taskbar, desktop, Start, …).
//! UWP applications are hosted by `ApplicationFrameHost.exe`; their real
//! process is found through the frame's `Windows.UI.Core.CoreWindow` child.
//!
//! Only `PROCESS_QUERY_LIMITED_INFORMATION` is requested, which works for
//! most processes without administrator rights. Nothing is injected and no
//! process memory is read.

use std::collections::HashMap;
use std::ffi::c_void;

use windows::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER, FILETIME, HANDLE, HWND, LPARAM, RECT,
};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, EnumWindows, GA_ROOTOWNER, GW_OWNER, GWL_EXSTYLE, GetAncestor, GetClassNameW,
    GetForegroundWindow, GetWindow, GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, IsIconic, IsWindowVisible, WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};
use windows::core::{BOOL, PCWSTR, PWSTR};

use super::{DetectError, ProcessIdentity, RawWindow, WindowSystem};
use crate::app_identity::ExeMetadata;

/// Window classes that belong to the Windows shell rather than to applications.
const SHELL_CLASSES: &[&str] = &[
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "Progman",
    "WorkerW",
    "NotifyIconOverflowWindow",
    "TopLevelWindowForOverflowXamlIsland",
    "XamlExplorerHostIslandWindow",
    "Shell_InputSwitchTopLevelWindow",
    "MultitaskingViewFrame",
    "ForegroundStaging",
    "TaskListThumbnailWnd",
    "Windows.UI.Core.CoreWindow",
    "Windows.UI.Composition.DesktopWindowContentBridge",
];

const FRAME_HOST_CLASS: &str = "ApplicationFrameWindow";
const CORE_WINDOW_CLASS: &str = "Windows.UI.Core.CoreWindow";

pub struct Win32WindowSystem {
    /// Last hosted process seen for each UWP frame window (keyed by handle).
    /// A minimized or suspended Store app detaches its CoreWindow from the
    /// frame; the cached PID keeps it listed, like a minimized Win32 app,
    /// until the frame closes.
    frame_pids: HashMap<isize, u32>,
}

impl Win32WindowSystem {
    pub fn new() -> Self {
        Self {
            frame_pids: HashMap::new(),
        }
    }
}

struct EnumContext<'a> {
    windows: Vec<RawWindow>,
    with_titles: bool,
    frame_pids: &'a mut HashMap<isize, u32>,
    seen_frames: Vec<isize>,
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    // SAFETY: buffer is valid for its length.
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..usize::try_from(len).unwrap_or(0).min(buf.len())])
}

/// Whether the window has a non-empty title, without reading the text (used
/// when window-title capture is off, so titles never enter our memory).
fn has_title(hwnd: HWND) -> bool {
    // SAFETY: plain query on a window handle.
    unsafe { GetWindowTextLengthW(hwnd) > 0 }
}

/// Whether `hwnd` is a visible window collapsed to (almost) zero size, as used
/// by some frameworks (e.g. classic Delphi VCL) for their taskbar window.
fn is_zero_size_visible(hwnd: HWND) -> bool {
    // SAFETY: plain queries on a window handle.
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return false;
        }
        let mut rect = RECT::default();
        GetWindowRect(hwnd, &mut rect).is_ok() && (rect.right - rect.left <= 1 || rect.bottom - rect.top <= 1)
    }
}

/// Reads a window title. Only called when window-title capture is enabled.
fn window_title(hwnd: HWND) -> Option<String> {
    // SAFETY: plain query on a window handle.
    let len = unsafe { GetWindowTextLengthW(hwnd) };
    if len <= 0 {
        return None;
    }
    let len = usize::try_from(len).unwrap_or(0).min(4096);
    let mut buf = vec![0u16; len + 1];
    // SAFETY: buffer is valid for its length.
    let copied = unsafe { GetWindowTextW(hwnd, &mut buf) };
    let copied = usize::try_from(copied).unwrap_or(0).min(buf.len());
    let title = String::from_utf16_lossy(&buf[..copied]);
    (!title.trim().is_empty()).then_some(title)
}

fn window_pid(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    // SAFETY: `pid` is a valid out pointer.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked = 0u32;
    // SAFETY: `cloaked` is a valid 4-byte out buffer.
    let result = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            (&mut cloaked as *mut u32).cast::<c_void>(),
            std::mem::size_of::<u32>() as u32,
        )
    };
    result.is_ok() && cloaked != 0
}

/// Returns the hosted UWP process for an `ApplicationFrameWindow`, if any.
fn uwp_child_pid(frame: HWND, frame_pid: u32) -> Option<u32> {
    struct Search {
        frame_pid: u32,
        found: Option<u32>,
    }
    unsafe extern "system" fn visit(child: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: lparam points at the `Search` owned by the caller for the duration of the call.
        let search = unsafe { &mut *(lparam.0 as *mut Search) };
        if class_name(child) == CORE_WINDOW_CLASS {
            let pid = window_pid(child);
            if pid != 0 && pid != search.frame_pid {
                search.found = Some(pid);
                return BOOL(0);
            }
        }
        BOOL(1)
    }
    let mut search = Search {
        frame_pid,
        found: None,
    };
    // SAFETY: the callback only dereferences `search`, which outlives the call.
    let _ = unsafe {
        EnumChildWindows(
            Some(frame),
            Some(visit),
            LPARAM(&mut search as *mut Search as isize),
        )
    };
    search.found
}

/// Resolves the process behind an `ApplicationFrameWindow`, remembering it so
/// the app stays identified while it is minimized or suspended.
fn resolve_frame_pid(frame: HWND, frame_pid: u32, cache: &mut HashMap<isize, u32>) -> u32 {
    let key = frame.0 as isize;
    match uwp_child_pid(frame, frame_pid) {
        Some(pid) => {
            cache.insert(key, pid);
            pid
        }
        // Without a cached PID (e.g. the app was already suspended when
        // DeskPresence started) this resolves to the frame host, which the
        // detector ignores.
        None => cache.get(&key).copied().unwrap_or(frame_pid),
    }
}

/// Applies the application-window filter. Returns `None` for windows that are
/// not user-facing application windows.
fn inspect(hwnd: HWND, with_titles: bool, frame_pids: &mut HashMap<isize, u32>) -> Option<RawWindow> {
    // SAFETY: simple queries on a handle supplied by EnumWindows.
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            return None;
        }
        let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let app_window = ex_style & WS_EX_APPWINDOW.0 != 0;
        if !app_window {
            if ex_style & WS_EX_TOOLWINDOW.0 != 0 || ex_style & WS_EX_NOACTIVATE.0 != 0 {
                return None;
            }
            // Owned windows are dialogs/popups of another window, except when
            // the owner is a visible zero-size taskbar window (legacy
            // frameworks); then the owned window is the real main window.
            if let Ok(owner) = GetWindow(hwnd, GW_OWNER)
                && !owner.is_invalid()
                && !is_zero_size_visible(owner)
            {
                return None;
            }
        }
        if is_cloaked(hwnd) {
            return None;
        }
        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_ok()
            && !IsIconic(hwnd).as_bool()
            && (rect.right - rect.left <= 1 || rect.bottom - rect.top <= 1)
        {
            return None;
        }
    }
    let class = class_name(hwnd);
    if SHELL_CLASSES.contains(&class.as_str()) {
        return None;
    }
    // SAFETY: plain query.
    let app_window = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 } & WS_EX_APPWINDOW.0 != 0;
    if !has_title(hwnd) && !app_window {
        return None;
    }

    let mut pid = window_pid(hwnd);
    if class == FRAME_HOST_CLASS {
        pid = resolve_frame_pid(hwnd, pid, frame_pids);
    }
    if pid == 0 {
        return None;
    }
    Some(RawWindow {
        handle: hwnd.0 as usize as u64,
        pid,
        // Titles are read only with the privacy opt-in.
        title: if with_titles { window_title(hwnd) } else { None },
    })
}

unsafe extern "system" fn enum_top_level(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: lparam points at the `EnumContext` owned by `top_level_windows`.
    let context = unsafe { &mut *(lparam.0 as *mut EnumContext<'_>) };
    if class_name(hwnd) == FRAME_HOST_CLASS {
        context.seen_frames.push(hwnd.0 as isize);
    }
    if let Some(window) = inspect(hwnd, context.with_titles, context.frame_pids) {
        context.windows.push(window);
    }
    BOOL(1)
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: we own this handle.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

fn filetime_to_unix_ms(ft: FILETIME) -> Option<u64> {
    let ticks = (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime);
    const EPOCH_DIFF_100NS: u64 = 116_444_736_000_000_000;
    ticks.checked_sub(EPOCH_DIFF_100NS).map(|t| t / 10_000)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Reads a string value from a version-info block.
fn ver_string(block: &[u8], sub_block: &str) -> Option<String> {
    let query = wide(sub_block);
    let mut ptr: *mut c_void = std::ptr::null_mut();
    let mut len = 0u32;
    // SAFETY: `block` is a complete version-info buffer; out pointers are valid.
    let ok = unsafe {
        VerQueryValueW(
            block.as_ptr().cast::<c_void>(),
            PCWSTR(query.as_ptr()),
            &mut ptr,
            &mut len,
        )
    };
    if !ok.as_bool() || ptr.is_null() || len == 0 {
        return None;
    }
    let block_start = block.as_ptr() as usize;
    let block_end = block_start + block.len();
    let start = ptr as usize;
    let chars = (len as usize).min(1024);
    // The returned pointer must lie inside our buffer.
    if start < block_start || start + chars * 2 > block_end {
        return None;
    }
    // SAFETY: bounds checked above; the data is UTF-16 inside `block`.
    let slice = unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), chars) };
    let end = slice.iter().position(|&c| c == 0).unwrap_or(slice.len());
    let value = String::from_utf16_lossy(&slice[..end]).trim().to_owned();
    (!value.is_empty()).then_some(value)
}

impl WindowSystem for Win32WindowSystem {
    fn name(&self) -> &'static str {
        "win32"
    }

    fn top_level_windows(&mut self, with_titles: bool) -> Result<Vec<RawWindow>, DetectError> {
        let mut context = EnumContext {
            windows: Vec::with_capacity(64),
            with_titles,
            frame_pids: &mut self.frame_pids,
            seen_frames: Vec::new(),
        };
        // SAFETY: the callback only dereferences `context`, which outlives the call.
        unsafe {
            EnumWindows(
                Some(enum_top_level),
                LPARAM(&mut context as *mut EnumContext<'_> as isize),
            )
        }
        .map_err(|e| DetectError::Os(e.to_string()))?;
        let EnumContext {
            windows, seen_frames, ..
        } = context;
        // Forget frames that no longer exist.
        self.frame_pids.retain(|frame, _| seen_frames.contains(frame));
        Ok(windows)
    }

    fn foreground_window(&mut self, with_title: bool) -> Option<RawWindow> {
        // SAFETY: plain queries.
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd.is_invalid() {
            return None;
        }
        // Owned dialogs and popups count as their owner application.
        let root = unsafe { GetAncestor(hwnd, GA_ROOTOWNER) };
        let root = if root.is_invalid() { hwnd } else { root };
        let class = class_name(root);
        if SHELL_CLASSES.contains(&class.as_str()) {
            return None;
        }
        let mut pid = window_pid(root);
        if class == FRAME_HOST_CLASS {
            pid = resolve_frame_pid(root, pid, &mut self.frame_pids);
        }
        (pid != 0).then(|| RawWindow {
            handle: root.0 as usize as u64,
            pid,
            title: if with_title { window_title(root) } else { None },
        })
    }

    fn process_identity(&mut self, pid: u32) -> Result<ProcessIdentity, DetectError> {
        // SAFETY: OpenProcess with limited query rights; the handle is closed by OwnedHandle.
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.map_err(|e| {
            let code = e.code();
            if code == ERROR_INVALID_PARAMETER.to_hresult() {
                DetectError::ProcessGone
            } else if code == ERROR_ACCESS_DENIED.to_hresult() {
                DetectError::AccessDenied
            } else {
                DetectError::Os(e.to_string())
            }
        })?;
        let handle = OwnedHandle(handle);

        let mut buf = vec![0u16; 32_768];
        let mut len = buf.len() as u32;
        // SAFETY: `buf` has `len` elements; `len` is updated with the written length.
        unsafe {
            QueryFullProcessImageNameW(handle.0, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len)
        }
        .map_err(|e| DetectError::Os(e.to_string()))?;
        let exe_path = String::from_utf16_lossy(&buf[..(len as usize).min(buf.len())]);
        if exe_path.is_empty() {
            return Err(DetectError::Os("empty image path".into()));
        }

        let (mut created, mut exited, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        // SAFETY: all out pointers are valid.
        let started_at_ms =
            unsafe { GetProcessTimes(handle.0, &mut created, &mut exited, &mut kernel, &mut user) }
                .ok()
                .and_then(|()| filetime_to_unix_ms(created));

        Ok(ProcessIdentity {
            pid,
            exe_path,
            started_at_ms,
        })
    }

    fn exe_metadata(&mut self, exe_path: &str) -> Option<ExeMetadata> {
        let path = wide(exe_path);
        // SAFETY: NUL-terminated path.
        let size = unsafe { GetFileVersionInfoSizeW(PCWSTR(path.as_ptr()), None) };
        if size == 0 || size > 16 * 1024 * 1024 {
            return None;
        }
        let mut block = vec![0u8; size as usize];
        // SAFETY: `block` has `size` bytes.
        unsafe {
            GetFileVersionInfoW(
                PCWSTR(path.as_ptr()),
                None,
                size,
                block.as_mut_ptr().cast::<c_void>(),
            )
        }
        .ok()?;

        // Language/code-page pairs declared by the file, then common fallbacks.
        let mut translations: Vec<(u16, u16)> = Vec::new();
        {
            let query = wide(r"\VarFileInfo\Translation");
            let mut ptr: *mut c_void = std::ptr::null_mut();
            let mut len = 0u32;
            // SAFETY: valid block and out pointers.
            let ok = unsafe {
                VerQueryValueW(
                    block.as_ptr().cast::<c_void>(),
                    PCWSTR(query.as_ptr()),
                    &mut ptr,
                    &mut len,
                )
            };
            let start = ptr as usize;
            let block_start = block.as_ptr() as usize;
            if ok.as_bool()
                && !ptr.is_null()
                && start >= block_start
                && start + len as usize <= block_start + block.len()
            {
                let count = (len as usize / 4).min(16);
                // SAFETY: bounds checked; each entry is two u16 values.
                let pairs = unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), count * 2) };
                translations.extend(pairs.chunks_exact(2).map(|p| (p[0], p[1])));
            }
        }
        translations.extend([(0x0409, 0x04B0), (0x0409, 0x04E4), (0x0000, 0x04B0)]);

        let lookup = |field: &str| {
            translations.iter().find_map(|(lang, cp)| {
                ver_string(&block, &format!(r"\StringFileInfo\{lang:04x}{cp:04x}\{field}"))
            })
        };
        let metadata = ExeMetadata {
            file_description: lookup("FileDescription"),
            product_name: lookup("ProductName"),
        };
        (metadata.file_description.is_some() || metadata.product_name.is_some()).then_some(metadata)
    }
}

#[cfg(test)]
mod tests {
    //! These run only on Windows (CI uses a Windows runner). They call the
    //! real Win32 APIs against stable targets: this test process and
    //! `explorer.exe`, which every desktop Windows installation has.
    use super::*;
    use crate::app_identity::normalize_path;

    fn explorer() -> String {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        format!(r"{root}\explorer.exe")
    }

    #[test]
    fn identifies_this_process() {
        let mut ws = Win32WindowSystem::new();
        let identity = ws.process_identity(std::process::id()).expect("own process");
        let exe = std::env::current_exe().expect("current exe");
        assert_eq!(
            normalize_path(&identity.exe_path),
            normalize_path(&exe.to_string_lossy())
        );
        let started = identity.started_at_ms.expect("start time");
        assert!(started > 1_577_836_800_000 && started <= crate::time::now_unix_ms());
    }

    #[test]
    fn nonexistent_process_is_an_error_not_a_panic() {
        let mut ws = Win32WindowSystem::new();
        assert!(ws.process_identity(0xFFFF_FFF0).is_err());
    }

    #[test]
    fn reads_version_metadata() {
        let metadata = Win32WindowSystem::new()
            .exe_metadata(&explorer())
            .expect("explorer metadata");
        assert!(metadata.file_description.is_some() || metadata.product_name.is_some());
    }

    #[test]
    fn missing_executables_have_no_metadata() {
        assert!(
            Win32WindowSystem::new()
                .exe_metadata(r"C:\definitely\not\here.exe")
                .is_none()
        );
    }

    #[test]
    fn enumeration_respects_title_privacy() {
        let mut ws = Win32WindowSystem::new();
        let windows = ws.top_level_windows(false).expect("EnumWindows");
        assert!(windows.iter().all(|w| w.title.is_none() && w.pid != 0));
        let _ = ws.foreground_window(false);
    }
}
