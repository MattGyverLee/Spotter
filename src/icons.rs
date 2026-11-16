//! Windows shell icons for the result list, one per file type. Icons are looked up by extension alone
//! (SHGFI_USEFILEATTRIBUTES), so no file is ever opened: a cloud placeholder is not downloaded and an
//! unreachable drive cannot stall the window. The cost is that `.exe` and `.lnk` files get the generic icon.

use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};
use std::collections::HashMap;
use std::mem::{size_of, zeroed};
use std::path::Path;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDIBits, GetObjectW, HBITMAP, HDC,
};
use windows_sys::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL};
use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows_sys::Win32::UI::Shell::{SHFILEINFOW, SHGFI_ICON, SHGFI_SMALLICON, SHGFI_USEFILEATTRIBUTES, SHGetFileInfoW};
use windows_sys::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO};

const FOLDER: &str = "<folder>";

/// Textures by lower-case extension. `None` means the shell had no icon, so it is not asked again.
pub struct Icons(HashMap<String, Option<TextureHandle>>);

impl Icons {
    pub fn new() -> Icons {
        // SHGetFileInfo needs COM on this thread; initialising it a second time is harmless.
        unsafe { CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32) };
        Icons(HashMap::new())
    }

    pub fn get(&mut self, ctx: &egui::Context, path: &Path, is_dir: bool) -> Option<&TextureHandle> {
        let key = if is_dir { FOLDER.to_string() } else { path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default() };
        self.0.entry(key).or_insert_with_key(|k| load(k).map(|img| ctx.load_texture(format!("icon:{k}"), img, TextureOptions::LINEAR))).as_ref()
    }
}

fn load(key: &str) -> Option<ColorImage> {
    let (name, attrs) = match key {
        FOLDER => ("folder".to_string(), FILE_ATTRIBUTE_DIRECTORY),
        "" => ("file".to_string(), FILE_ATTRIBUTE_NORMAL),
        ext => (format!("file.{ext}"), FILE_ATTRIBUTE_NORMAL),
    };
    let wide: Vec<u16> = name.encode_utf16().chain([0]).collect();
    unsafe {
        let mut info: SHFILEINFOW = zeroed();
        let flags = SHGFI_ICON | SHGFI_SMALLICON | SHGFI_USEFILEATTRIBUTES;
        if SHGetFileInfoW(wide.as_ptr(), attrs, &mut info, size_of::<SHFILEINFOW>() as u32, flags) == 0 || info.hIcon.is_null() {
            return None;
        }
        let img = to_image(info.hIcon);
        DestroyIcon(info.hIcon);
        img
    }
}

/// The icon's pixels. Icons with an alpha channel use it; older ones take their transparency from the mask.
fn to_image(icon: HICON) -> Option<ColorImage> {
    unsafe {
        let mut ii: ICONINFO = zeroed();
        if GetIconInfo(icon, &mut ii) == 0 {
            return None;
        }
        let img = if ii.hbmColor.is_null() { None } else { pixels(ii.hbmColor, ii.hbmMask) };
        DeleteObject(ii.hbmColor);
        DeleteObject(ii.hbmMask);
        img
    }
}

fn pixels(color: HBITMAP, mask: HBITMAP) -> Option<ColorImage> {
    unsafe {
        let mut bm: BITMAP = zeroed();
        if GetObjectW(color, size_of::<BITMAP>() as i32, (&mut bm as *mut BITMAP).cast()) == 0 {
            return None;
        }
        let (w, h) = (bm.bmWidth, bm.bmHeight);
        let dc = CreateCompatibleDC(null_mut());
        let px = bgra(dc, color, w, h);
        let mask = bgra(dc, mask, w, h);
        DeleteDC(dc);
        let mut px = px?;
        let has_alpha = px.chunks_exact(4).any(|p| p[3] != 0);
        for (i, p) in px.chunks_exact_mut(4).enumerate() {
            p.swap(0, 2);
            if !has_alpha {
                p[3] = if mask.as_ref().is_some_and(|m| m[i * 4] != 0) { 0 } else { 255 };
            }
        }
        Some(ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &px))
    }
}

/// A bitmap as top-down 32-bit BGRA.
fn bgra(dc: HDC, bmp: HBITMAP, w: i32, h: i32) -> Option<Vec<u8>> {
    unsafe {
        let mut bi: BITMAPINFO = zeroed();
        bi.bmiHeader = BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB, ..zeroed() };
        let mut buf = vec![0u8; (w * h * 4) as usize];
        (GetDIBits(dc, bmp, 0, h as u32, buf.as_mut_ptr().cast(), &mut bi, DIB_RGB_COLORS) != 0).then_some(buf)
    }
}
