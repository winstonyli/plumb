//! Explorer's own icons for files and folders, fetched by extension so no file is touched.
use windows::core::PCWSTR;
use windows::Win32::Graphics::Gdi::{
    DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HGDIOBJ,
};
use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL};
use windows::Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGFI_SMALLICON, SHGFI_USEFILEATTRIBUTES};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, ICONINFO};

/// What decides a node's icon: all folders share one; a file goes by its lowercased extension
/// (empty for none). Names with a different extension but the same type share the shell's icon cache.
pub fn key(name: &str, dir: bool) -> String {
    if dir {
        return String::new();
    }
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => ext.to_lowercase(),
        _ => String::new(),
    }
}

/// A premultiplied BGRA image, `size` pixels square.
pub struct Image {
    pub size: u32,
    pub bgra: Vec<u8>,
}

/// The shell icon for a file with extension `key` (or a folder), small (16 px) or large (32 px).
pub fn load(key: &str, dir: bool, large: bool) -> Option<Image> {
    let path: Vec<u16> = format!("x.{key}").encode_utf16().chain([0]).collect();
    let attrs = if dir { FILE_ATTRIBUTE_DIRECTORY } else { FILE_ATTRIBUTE_NORMAL };
    let flags = SHGFI_ICON | SHGFI_USEFILEATTRIBUTES | if large { SHGFI_LARGEICON } else { SHGFI_SMALLICON };
    let mut info = SHFILEINFOW::default();
    unsafe {
        if SHGetFileInfoW(PCWSTR(path.as_ptr()), attrs, Some(&mut info), size_of::<SHFILEINFOW>() as u32, flags) == 0 || info.hIcon.is_invalid() {
            return None;
        }
        let image = bits(&info.hIcon);
        let _ = DestroyIcon(info.hIcon);
        image
    }
}

unsafe fn bits(icon: &windows::Win32::UI::WindowsAndMessaging::HICON) -> Option<Image> {
    unsafe {
        let mut ii = ICONINFO::default();
        GetIconInfo(*icon, &mut ii).ok()?;
        let image = (|| {
            let mut bm = BITMAP::default();
            GetObjectW(HGDIOBJ(ii.hbmColor.0), size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut _));
            let size = bm.bmWidth as u32;
            if size == 0 || bm.bmHeight as u32 != size {
                return None;
            }
            let dc = GetDC(None);
            let read = |hbm| {
                let mut bmi = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: size as i32,
                        biHeight: -(size as i32), // top-down
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: BI_RGB.0,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                let mut px = vec![0u8; (size * size * 4) as usize];
                let n = GetDIBits(dc, hbm, 0, size, Some(px.as_mut_ptr().cast()), &mut bmi, DIB_RGB_COLORS);
                (n != 0).then_some(px)
            };
            let color = read(ii.hbmColor);
            let mask = read(ii.hbmMask);
            ReleaseDC(None, dc);
            let mut px = color?;
            if px.as_chunks::<4>().0.iter().all(|p| p[3] == 0) {
                // An old-style icon without an alpha channel: the AND mask says which pixels are see-through.
                let mask = mask?;
                for (p, m) in px.as_chunks_mut::<4>().0.iter_mut().zip(mask.as_chunks::<4>().0) {
                    p[3] = if m[0] == 0 { 255 } else { 0 };
                }
            }
            premultiply(&mut px);
            Some(Image { size, bgra: px })
        })();
        let _ = DeleteObject(HGDIOBJ(ii.hbmColor.0));
        let _ = DeleteObject(HGDIOBJ(ii.hbmMask.0));
        image
    }
}

fn premultiply(px: &mut [u8]) {
    for p in px.as_chunks_mut::<4>().0 {
        let a = p[3] as u32;
        for c in &mut p[..3] {
            *c = ((*c as u32 * a + 127) / 255) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_is_the_lowercase_extension_and_folders_share_one() {
        assert_eq!(key("Photo.JPG", false), "jpg");
        assert_eq!(key("a.tar.gz", false), "gz");
        assert_eq!(key("Makefile", false), "");
        assert_eq!(key(".gitignore", false), "");
        assert_eq!(key("trailing.", false), "");
        assert_eq!(key("v1.2", true), "");
    }

    #[test]
    fn the_shell_gives_a_visible_icon_for_a_folder_and_a_file_type() {
        for (k, dir) in [("", true), ("txt", false), ("zzzunknown", false)] {
            let img = load(k, dir, false).unwrap_or_else(|| panic!("no icon for {k:?}"));
            assert_eq!(img.size, 16);
            assert!(img.bgra.as_chunks::<4>().0.iter().any(|p| p[3] > 0), "{k:?} is fully transparent");
        }
        assert_eq!(load("txt", false, true).unwrap().size, 32);
    }
}
