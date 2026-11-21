//! Windows Shell file-type icon lookup using `SHGFI_USEFILEATTRIBUTES` (never touches CSV paths on disk).
use std::{
    collections::{HashMap, VecDeque},
    sync::Mutex,
};

pub const DIR_ICON_KEY: &str = "__dir__";
const MAX_CACHED_ICONS: usize = 512;
const MAX_KEYS_PER_REQUEST: usize = 256;
const MAX_EXTENSION_LEN: usize = 64;

/// Normalize a requested icon key into `"__dir__"`, `""` (generic file), or lowercase `".ext"`.
pub fn normalize_key(raw: &str) -> String {
    if raw == DIR_ICON_KEY {
        return DIR_ICON_KEY.to_owned();
    }
    if raw.is_empty() || raw.len() > MAX_EXTENSION_LEN + 1 || !raw.starts_with('.') {
        return String::new();
    }
    let suffix = &raw[1..];
    if suffix.is_empty()
        || suffix.contains(['\\', '/', ':', '\0', '*', '?', '"', '<', '>', '|', '.'])
    {
        return String::new();
    }
    let mut out = String::with_capacity(raw.len());
    out.push('.');
    if suffix.is_ascii() {
        out.push_str(suffix);
        out[1..].make_ascii_lowercase();
    } else {
        out.push_str(&suffix.to_lowercase());
    }
    out
}

#[derive(Default)]
struct CacheInner {
    entries: HashMap<String, Option<String>>,
    order: VecDeque<String>,
}

/// Process-local, memory-bounded FIFO cache of encoded system file-type icons.
#[derive(Default)]
pub struct IconCache {
    inner: Mutex<CacheInner>,
}

impl IconCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up already-cached entries; returns `(resolved_map, uncached_normalized_keys)`.
    pub fn split_cached(&self, keys: &[String]) -> (HashMap<String, String>, Vec<String>) {
        let mut resolved = HashMap::new();
        let mut missing = Vec::new();
        let Ok(cache) = self.inner.lock() else {
            return (resolved, missing);
        };
        let mut seen_missing = HashMap::<String, ()>::new();
        for raw in keys.iter().take(MAX_KEYS_PER_REQUEST) {
            let norm = normalize_key(raw);
            if let Some(entry) = cache.entries.get(&norm) {
                if let Some(data_url) = entry {
                    resolved.insert(raw.clone(), data_url.clone());
                }
            } else if seen_missing.insert(norm.clone(), ()).is_none() {
                missing.push(norm);
            }
        }
        (resolved, missing)
    }

    /// Resolve any uncached keys using Windows Shell file-type associations and store them in the bounded cache.
    pub fn resolve_missing(
        &self,
        keys: &[String],
        missing: Vec<String>,
        mut resolved: HashMap<String, String>,
    ) -> HashMap<String, String> {
        let fetched = fetch_system_icons(&missing);
        if let Ok(mut cache) = self.inner.lock() {
            for (norm, icon) in fetched {
                if !cache.entries.contains_key(&norm) {
                    while cache.entries.len() >= MAX_CACHED_ICONS {
                        if let Some(evicted) = cache.order.pop_front() {
                            cache.entries.remove(&evicted);
                        } else {
                            break;
                        }
                    }
                    cache.order.push_back(norm.clone());
                }
                cache.entries.insert(norm, icon);
            }
            for raw in keys.iter().take(MAX_KEYS_PER_REQUEST) {
                if !resolved.contains_key(raw) {
                    let norm = normalize_key(raw);
                    if let Some(Some(data_url)) = cache.entries.get(&norm) {
                        resolved.insert(raw.clone(), data_url.clone());
                    }
                }
            }
        }
        resolved
    }
}

#[cfg(windows)]
fn fetch_system_icons(normalized_keys: &[String]) -> Vec<(String, Option<String>)> {
    use base64::Engine;
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::{
        Foundation::{RPC_E_CHANGED_MODE, S_FALSE, S_OK},
        Graphics::Gdi::{
            CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, GetObjectW, BITMAP, BITMAPINFO,
            BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HDC, HGDIOBJ,
        },
        Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL},
        System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED},
        UI::{
            Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGFI_USEFILEATTRIBUTES},
            WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO},
        },
    };

    struct ComGuard(bool);
    impl Drop for ComGuard {
        fn drop(&mut self) {
            if self.0 {
                unsafe { CoUninitialize() };
            }
        }
    }
    struct IconGuard(HICON);
    impl Drop for IconGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { DestroyIcon(self.0) };
            }
        }
    }
    struct GdiObjGuard(HGDIOBJ);
    impl Drop for GdiObjGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { DeleteObject(self.0) };
            }
        }
    }
    struct DcGuard(HDC);
    impl Drop for DcGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { DeleteDC(self.0) };
            }
        }
    }

    fn encode_rgba_png(width: u32, height: u32, rgba: &[u8]) -> Option<String> {
        let mut png_bytes = Vec::with_capacity(2048);
        {
            let mut encoder = png::Encoder::new(&mut png_bytes, width, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.set_compression(png::Compression::Fast);
            let mut writer = encoder.write_header().ok()?;
            writer.write_image_data(rgba).ok()?;
        }
        let mut url = String::with_capacity(22 + png_bytes.len().div_ceil(3) * 4);
        url.push_str("data:image/png;base64,");
        base64::engine::general_purpose::STANDARD.encode_string(&png_bytes, &mut url);
        Some(url)
    }

    fn hicon_to_data_url(hicon: HICON) -> Option<String> {
        let mut info: ICONINFO = unsafe { std::mem::zeroed() };
        if unsafe { GetIconInfo(hicon, &mut info) } == 0 {
            return None;
        }
        let _color_guard = GdiObjGuard(info.hbmColor as HGDIOBJ);
        let _mask_guard = GdiObjGuard(info.hbmMask as HGDIOBJ);

        let mut bmp: BITMAP = unsafe { std::mem::zeroed() };
        let (width, height, has_color) = if !info.hbmColor.is_null() {
            if unsafe {
                GetObjectW(
                    info.hbmColor as HGDIOBJ,
                    std::mem::size_of::<BITMAP>() as i32,
                    &mut bmp as *mut _ as *mut _,
                )
            } == 0
            {
                return None;
            }
            (bmp.bmWidth as u32, bmp.bmHeight.unsigned_abs(), true)
        } else if !info.hbmMask.is_null() {
            if unsafe {
                GetObjectW(
                    info.hbmMask as HGDIOBJ,
                    std::mem::size_of::<BITMAP>() as i32,
                    &mut bmp as *mut _ as *mut _,
                )
            } == 0
            {
                return None;
            }
            (bmp.bmWidth as u32, bmp.bmHeight.unsigned_abs() / 2, false)
        } else {
            return None;
        };

        if width == 0 || height == 0 || width > 256 || height > 256 {
            return None;
        }

        let hdc = DcGuard(unsafe { CreateCompatibleDC(std::ptr::null_mut()) });
        if hdc.0.is_null() {
            return None;
        }

        let make_bmi = |h: u32| -> BITMAPINFO {
            let mut bmi: BITMAPINFO = unsafe { std::mem::zeroed() };
            bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bmi.bmiHeader.biWidth = width as i32;
            bmi.bmiHeader.biHeight = -(h as i32); // top-down DIB
            bmi.bmiHeader.biPlanes = 1;
            bmi.bmiHeader.biBitCount = 32;
            bmi.bmiHeader.biCompression = BI_RGB;
            bmi
        };

        let byte_len = (width as usize) * (height as usize) * 4;
        let mut pixels = vec![0u8; byte_len];

        if has_color {
            let mut bmi = make_bmi(height);
            let lines = unsafe {
                GetDIBits(
                    hdc.0,
                    info.hbmColor,
                    0,
                    height,
                    pixels.as_mut_ptr() as *mut _,
                    &mut bmi,
                    DIB_RGB_COLORS,
                )
            };
            if lines == 0 {
                return None;
            }
            let has_alpha = pixels.chunks_exact(4).any(|p| p[3] != 0);
            if has_alpha {
                for p in pixels.chunks_exact_mut(4) {
                    p.swap(0, 2); // BGRA -> RGBA
                }
            } else if !info.hbmMask.is_null() {
                // Legacy icon without embedded alpha channel: derive transparency from AND mask.
                let mut mask = vec![0u8; byte_len];
                let mut mask_bmi = make_bmi(height);
                let mask_lines = unsafe {
                    GetDIBits(
                        hdc.0,
                        info.hbmMask,
                        0,
                        height,
                        mask.as_mut_ptr() as *mut _,
                        &mut mask_bmi,
                        DIB_RGB_COLORS,
                    )
                };
                if mask_lines == 0 {
                    return None;
                }
                for (p, m) in pixels.chunks_exact_mut(4).zip(mask.chunks_exact(4)) {
                    if m[0] == 0 {
                        p.swap(0, 2);
                        p[3] = 255;
                    } else {
                        p.copy_from_slice(&[0, 0, 0, 0]);
                    }
                }
            } else {
                for p in pixels.chunks_exact_mut(4) {
                    p.swap(0, 2);
                    p[3] = 255;
                }
            }
        } else {
            // Monochrome icon: hbmMask holds top half = AND mask, bottom half = XOR mask.
            let mut mask = vec![0u8; byte_len * 2];
            let mut mask_bmi = make_bmi(height * 2);
            let mask_lines = unsafe {
                GetDIBits(
                    hdc.0,
                    info.hbmMask,
                    0,
                    height * 2,
                    mask.as_mut_ptr() as *mut _,
                    &mut mask_bmi,
                    DIB_RGB_COLORS,
                )
            };
            if mask_lines == 0 {
                return None;
            }
            let (and_half, xor_half) = mask.split_at(byte_len);
            for ((dst, and_px), xor_px) in pixels
                .chunks_exact_mut(4)
                .zip(and_half.chunks_exact(4))
                .zip(xor_half.chunks_exact(4))
            {
                if and_px[0] == 0 {
                    let v = xor_px[0];
                    dst.copy_from_slice(&[v, v, v, 255]);
                } else if xor_px[0] != 0 {
                    dst.copy_from_slice(&[0, 0, 0, 255]);
                }
            }
        }

        encode_rgba_png(width, height, &pixels)
    }

    let hr = unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) };
    let _com_guard = ComGuard(hr == S_OK || hr == S_FALSE);
    let _ = RPC_E_CHANGED_MODE;

    let mut results = Vec::with_capacity(normalized_keys.len());
    for key in normalized_keys {
        let (dummy, attrs) = if key == DIR_ICON_KEY {
            ("folder".to_owned(), FILE_ATTRIBUTE_DIRECTORY)
        } else if key.is_empty() {
            ("file".to_owned(), FILE_ATTRIBUTE_NORMAL)
        } else {
            (format!("file{key}"), FILE_ATTRIBUTE_NORMAL)
        };
        let wide: Vec<u16> = OsStr::new(&dummy).encode_wide().chain(Some(0)).collect();
        let mut shfi: SHFILEINFOW = unsafe { std::mem::zeroed() };
        let ok = unsafe {
            SHGetFileInfoW(
                wide.as_ptr(),
                attrs,
                &mut shfi,
                std::mem::size_of::<SHFILEINFOW>() as u32,
                SHGFI_ICON | SHGFI_LARGEICON | SHGFI_USEFILEATTRIBUTES,
            )
        };
        let data_url = if ok != 0 && !shfi.hIcon.is_null() {
            let icon = IconGuard(shfi.hIcon);
            hicon_to_data_url(icon.0)
        } else {
            None
        };
        results.push((key.clone(), data_url));
    }
    results
}

#[cfg(not(windows))]
fn fetch_system_icons(normalized_keys: &[String]) -> Vec<(String, Option<String>)> {
    normalized_keys.iter().map(|k| (k.clone(), None)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_and_sanitizes_icon_keys() {
        assert_eq!(normalize_key("__dir__"), "__dir__");
        assert_eq!(normalize_key(""), "");
        assert_eq!(normalize_key(".DLL"), ".dll");
        assert_eq!(normalize_key(".Ps1"), ".ps1");
        assert_eq!(normalize_key("no-dot"), "");
        assert_eq!(normalize_key("."), "");
        assert_eq!(normalize_key(".a\\b"), "");
        assert_eq!(normalize_key(".a/b"), "");
        assert_eq!(normalize_key(".tar.gz"), "");
        assert_eq!(normalize_key(&format!(".{}", "x".repeat(65))), "");
    }

    #[cfg(windows)]
    #[test]
    fn extracts_valid_png_icons_without_touching_disk_and_caches_results() {
        let cache = IconCache::new();
        let keys = vec![
            "__dir__".to_owned(),
            "".to_owned(),
            ".exe".to_owned(),
            ".DLL".to_owned(),
            ".txt".to_owned(),
        ];
        let (resolved, missing) = cache.split_cached(&keys);
        assert!(resolved.is_empty());
        assert_eq!(missing.len(), 5);

        let icons = cache.resolve_missing(&keys, missing, resolved);
        for key in &keys {
            let url = icons
                .get(key)
                .unwrap_or_else(|| panic!("expected system icon for {key:?}"));
            assert!(url.starts_with("data:image/png;base64,"), "{key:?}");
            assert!(url.len() > 80, "{key:?} icon too small");
        }

        // Second lookup is served entirely from cache (case-normalized too).
        let keys2 = vec![".dll".to_owned(), "__dir__".to_owned()];
        let (resolved2, missing2) = cache.split_cached(&keys2);
        assert!(missing2.is_empty());
        assert_eq!(resolved2.get(".dll"), icons.get(".DLL"));
        assert_eq!(resolved2.get("__dir__"), icons.get("__dir__"));
    }
}
