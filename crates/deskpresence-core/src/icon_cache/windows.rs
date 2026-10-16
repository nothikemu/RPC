//! Win32 icon extraction: `SHDefExtractIconW` for a sized icon from the
//! executable's resources, falling back to `SHGetFileInfoW` (the shell's icon,
//! e.g. the generic application icon). The HICON is converted to RGBA with
//! `GetDIBits`; icons without an alpha channel use their AND mask.

use std::ffi::c_void;

use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject,
    GetDIBits, GetObjectW, HBITMAP, HDC, HGDIOBJ,
};
use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL;
use windows::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
};
use windows::Win32::UI::Shell::{
    SHDefExtractIconW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGetFileInfoW,
};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO};
use windows::core::PCWSTR;

use super::{IconError, IconExtractor, RgbaImage};

pub struct Win32IconExtractor {
    com_initialized: bool,
}

impl Win32IconExtractor {
    pub fn new() -> Self {
        // SAFETY: called once on the icon thread; balanced in Drop.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
        Self {
            com_initialized: hr.is_ok(),
        }
    }
}

impl Drop for Win32IconExtractor {
    fn drop(&mut self) {
        if self.com_initialized {
            // SAFETY: balances the successful CoInitializeEx on this thread.
            unsafe { CoUninitialize() };
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

struct IconGuard(HICON);

impl Drop for IconGuard {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: we own this icon handle.
            let _ = unsafe { DestroyIcon(self.0) };
        }
    }
}

struct BitmapGuard(HBITMAP);

impl Drop for BitmapGuard {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: GetIconInfo transfers ownership of these bitmaps to us.
            let _ = unsafe { DeleteObject(HGDIOBJ(self.0.0)) };
        }
    }
}

struct DcGuard(HDC);

impl Drop for DcGuard {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: created by CreateCompatibleDC.
            let _ = unsafe { DeleteDC(self.0) };
        }
    }
}

impl IconExtractor for Win32IconExtractor {
    fn extract(&mut self, exe_path: &str, size: u32) -> Result<RgbaImage, IconError> {
        let path = wide(exe_path);
        let mut large = HICON::default();
        // SAFETY: `path` is NUL-terminated and outlives the call; `large` is a valid out pointer.
        let hr =
            unsafe { SHDefExtractIconW(PCWSTR(path.as_ptr()), 0, 0, Some(&mut large), None, size & 0xFFFF) };
        if hr.is_ok() && !large.is_invalid() {
            let guard = IconGuard(large);
            if let Ok(image) = hicon_to_rgba(guard.0) {
                return Ok(image);
            }
        }

        let mut info = SHFILEINFOW::default();
        // SAFETY: `info` is a valid, correctly sized out structure.
        let ok = unsafe {
            SHGetFileInfoW(
                PCWSTR(path.as_ptr()),
                FILE_ATTRIBUTE_NORMAL,
                Some(&mut info),
                std::mem::size_of::<SHFILEINFOW>() as u32,
                SHGFI_ICON | SHGFI_LARGEICON,
            )
        };
        if ok == 0 || info.hIcon.is_invalid() {
            return Err(IconError::NoIcon);
        }
        let guard = IconGuard(info.hIcon);
        hicon_to_rgba(guard.0)
    }
}

/// Converts an icon to straight-alpha RGBA.
fn hicon_to_rgba(icon: HICON) -> Result<RgbaImage, IconError> {
    let mut info = ICONINFO::default();
    // SAFETY: `icon` is a valid handle; `info` is a valid out pointer.
    unsafe { GetIconInfo(icon, &mut info) }.map_err(|e| IconError::Os(e.to_string()))?;
    let color = BitmapGuard(info.hbmColor);
    let mask = BitmapGuard(info.hbmMask);
    if color.0.is_invalid() {
        return Err(IconError::InvalidImage(
            "monochrome icons are not supported".into(),
        ));
    }

    let mut bitmap = BITMAP::default();
    // SAFETY: `bitmap` is a valid out buffer of the size passed.
    let written = unsafe {
        GetObjectW(
            HGDIOBJ(color.0.0),
            std::mem::size_of::<BITMAP>() as i32,
            Some((&mut bitmap as *mut BITMAP).cast::<c_void>()),
        )
    };
    if written == 0 {
        return Err(IconError::Os("GetObjectW failed".into()));
    }
    let width = u32::try_from(bitmap.bmWidth).unwrap_or(0);
    let height = bitmap.bmHeight.unsigned_abs();
    if width == 0 || height == 0 || width > 256 || height > 256 {
        return Err(IconError::InvalidImage(format!(
            "unexpected size {width}x{height}"
        )));
    }

    // SAFETY: a memory DC compatible with the screen.
    let dc = DcGuard(unsafe { CreateCompatibleDC(None) });
    if dc.0.is_invalid() {
        return Err(IconError::Os("CreateCompatibleDC failed".into()));
    }

    let mut pixels = read_bits(&dc, color.0, width, height)?;
    let has_alpha = pixels.chunks_exact(4).any(|p| p[3] != 0);
    if !has_alpha {
        // Legacy icon: derive alpha from the AND mask (black = opaque).
        let mask_pixels = if mask.0.is_invalid() {
            None
        } else {
            read_bits(&dc, mask.0, width, height).ok()
        };
        for (i, pixel) in pixels.chunks_exact_mut(4).enumerate() {
            let transparent = mask_pixels
                .as_ref()
                .is_some_and(|m| m.get(i * 4).copied().unwrap_or(0) != 0);
            pixel[3] = if transparent { 0 } else { 255 };
        }
    }
    // BGRA → RGBA.
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Ok(RgbaImage {
        width,
        height,
        pixels,
    })
}

/// Reads a bitmap as 32-bit top-down BGRA.
fn read_bits(dc: &DcGuard, bitmap: HBITMAP, width: u32, height: u32) -> Result<Vec<u8>, IconError> {
    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut buffer = vec![0u8; (width as usize) * (height as usize) * 4];
    // SAFETY: `buffer` holds exactly width*height 32-bit pixels as described by `info`.
    let lines = unsafe {
        GetDIBits(
            dc.0,
            bitmap,
            0,
            height,
            Some(buffer.as_mut_ptr().cast::<c_void>()),
            &mut info,
            DIB_RGB_COLORS,
        )
    };
    if lines != height as i32 {
        return Err(IconError::Os(format!(
            "GetDIBits copied {lines} of {height} lines"
        )));
    }
    Ok(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_a_valid_icon_from_explorer() {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let mut extractor = Win32IconExtractor::new();
        let image = extractor
            .extract(&format!(r"{root}\explorer.exe"), super::super::ICON_SIZE)
            .expect("explorer icon");
        assert!(image.width > 0 && image.height > 0 && image.width <= 256);
        assert_eq!(image.pixels.len(), (image.width * image.height * 4) as usize);
        assert!(
            image.pixels.chunks_exact(4).any(|p| p[3] != 0),
            "icon is not fully transparent"
        );
        let png = super::super::encode_png(&image).expect("encodes");
        super::super::validate_png(&png).expect("valid png");
    }

    #[test]
    fn missing_file_falls_back_or_errors_without_panicking() {
        let mut extractor = Win32IconExtractor::new();
        // The shell may return a generic icon for unknown paths; either outcome is fine.
        let _ = extractor.extract(r"C:\definitely\not\here.exe", 48);
    }
}
