// Кратко: всё, что про Windows — стекло окна, иконки ярлыков, запуск, путь к рабочему столу.

use std::path::{Path, PathBuf};

use slint::{Image, Rgba8Pixel, SharedPixelBuffer};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{GENERIC_READ, HWND, RECT, SIZE};
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RRF_RT_REG_SZ};
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub fn com_init() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
}

pub fn desktop_dir() -> PathBuf {
    // dev: GF_DESKTOP=dir uses a demo folder instead of the real Desktop (screenshots, GIFs)
    if let Ok(d) = std::env::var("GF_DESKTOP") {
        return PathBuf::from(d);
    }
    unsafe {
        if let Ok(p) = SHGetKnownFolderPath(&FOLDERID_Desktop, KF_FLAG_DEFAULT, None) {
            let s = p.to_string().unwrap_or_default();
            CoTaskMemFree(Some(p.0 as _));
            if !s.is_empty() {
                return PathBuf::from(s);
            }
        }
    }
    PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default()).join("Desktop")
}

pub fn system_dark() -> bool {
    let mut v: u32 = 1;
    let mut sz = 4u32;
    unsafe {
        let _ = RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut v as *mut _ as _),
            Some(&mut sz),
        );
    }
    v == 0
}

/// Panel: real Win11 acrylic. Hidden DWM frame extended over the client area
///   (DWM then reads our per-pixel alpha) + system backdrop. DWM makes it solid when inactive;
///   fine, the panel closes on focus loss.
/// Tile: same acrylic. Win11 makes backdrops solid when the window's NC state is
///   inactive; tiles never activate, so plat.rs answers WM_NCACTIVATE as always-active -> backdrop
///   stays live without taking focus (spike 2026-11-22, examples/spike.rs). wallpaper_blur = fallback.
pub fn glass(hwnd: HWND, dark: bool) {
    unsafe {
        let d: i32 = dark as i32;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &d as *const _ as _, 4);
        // our own grow / fold animation only: DWM's show/hide transition ran on top of it
        let off: i32 = 1;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_TRANSITIONS_FORCEDISABLED, &off as *const _ as _, 4);
        let c = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &c as *const _ as _, 4);
        let m = MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 };
        let _ = DwmExtendFrameIntoClientArea(hwnd, &m);
        let b = DWMSBT_TRANSIENTWINDOW;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &b as *const _ as _, 4);
        // Active window: DWM paints the caption band over the extended frame (lighter strip on top).
        // Tiles are faked NC-active (plat.rs WM_NCACTIVATE), the panel is really active: same band.
        // No caption color = no band.
        let none: u32 = 0xFFFF_FFFE; // DWMWA_COLOR_NONE
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_CAPTION_COLOR, &none as *const _ as _, 4);
    }
}

/// Frosted copy of the desktop wallpaper under screen rect `r` (primary monitor), acrylic-like tint.
/// glass: 0 dense .. 2 clear (how much of the wallpaper shows through the tint).
pub fn wallpaper_blur(r: RECT, dark: bool, glass: i32) -> Option<Image> {
    const DOWN: f64 = 4.0; // work at 1/4 size: cheap, and blur hides it
    const MARGIN: i32 = 6; // extra pixels so blur near edges uses real neighbours
    unsafe {
        let mut wp = [0u16; 260];
        SystemParametersInfoW(SPI_GETDESKWALLPAPER, wp.len() as u32, Some(wp.as_mut_ptr() as _), Default::default()).ok()?;
        let len = wp.iter().position(|&c| c == 0).unwrap_or(0);
        if len == 0 {
            return None;
        }
        let path = HSTRING::from(String::from_utf16_lossy(&wp[..len]));
        let f: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let dec = f.CreateDecoderFromFilename(&path, None, GENERIC_READ, WICDecodeMetadataCacheOnDemand).ok()?;
        let frame = dec.GetFrame(0).ok()?;
        let (mut iw, mut ih) = (0u32, 0u32);
        frame.GetSize(&mut iw, &mut ih).ok()?;

        // Wallpaper placement on screen (registry WallpaperStyle): 2 stretch, 6 fit, else fill.
        let sw = GetSystemMetrics(SM_CXSCREEN) as f64 / DOWN;
        let sh = GetSystemMetrics(SM_CYSCREEN) as f64 / DOWN;
        let (kx, ky) = (sw / iw as f64, sh / ih as f64);
        let (sx, sy) = match wallpaper_style().as_str() {
            "2" => (kx, ky),
            "6" => (kx.min(ky), kx.min(ky)),
            _ => (kx.max(ky), kx.max(ky)),
        };
        let tw = ((iw as f64 * sx).round() as u32).max(1);
        let th = ((ih as f64 * sy).round() as u32).max(1);
        let ox = (tw as f64 - sw) / 2.0;
        let oy = (th as f64 - sh) / 2.0;

        let scaler = f.CreateBitmapScaler().ok()?;
        scaler.Initialize(&frame, tw, th, WICBitmapInterpolationModeFant).ok()?;
        let conv = f.CreateFormatConverter().ok()?;
        conv.Initialize(&scaler, &GUID_WICPixelFormat32bppRGBA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)
            .ok()?;

        // tile rect in scaled-wallpaper coords
        let tx0 = (r.left as f64 / DOWN + ox).floor() as i32;
        let ty0 = (r.top as f64 / DOWN + oy).floor() as i32;
        let tx1 = (r.right as f64 / DOWN + ox).ceil() as i32;
        let ty1 = (r.bottom as f64 / DOWN + oy).ceil() as i32;
        let x0 = (tx0 - MARGIN).clamp(0, tw as i32 - 1);
        let y0 = (ty0 - MARGIN).clamp(0, th as i32 - 1);
        let x1 = (tx1 + MARGIN).clamp(x0 + 1, tw as i32);
        let y1 = (ty1 + MARGIN).clamp(y0 + 1, th as i32);
        let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
        let mut buf = vec![0u8; w * h * 4];
        let rect = WICRect { X: x0, Y: y0, Width: w as i32, Height: h as i32 };
        conv.CopyPixels(&rect, (w * 4) as u32, &mut buf).ok()?;

        for _ in 0..3 {
            box_blur(&mut buf, w, h, 3); // 3 box passes ~ gaussian
        }

        // crop to tile, add acrylic tint (luminosity layer)
        let cx = (tx0 - x0).clamp(0, w as i32 - 1) as usize;
        let cy = (ty0 - y0).clamp(0, h as i32 - 1) as usize;
        let cw = ((tx1 - tx0) as usize).min(w - cx).max(1);
        let ch = ((ty1 - ty0) as usize).min(h - cy).max(1);
        let k = [0.55, 0.32, 0.12][glass.clamp(0, 2) as usize];
        let tint = if dark { 32.0 } else { 243.0 };
        let mut pb = SharedPixelBuffer::<Rgba8Pixel>::new(cw as u32, ch as u32);
        for (i, px) in pb.make_mut_slice().iter_mut().enumerate() {
            let (x, y) = (cx + i % cw, cy + i / cw);
            let s = &buf[(y * w + x) * 4..][..4];
            let mix = |v: u8| (v as f64 * (1.0 - k) + tint * k) as u8;
            *px = Rgba8Pixel { r: mix(s[0]), g: mix(s[1]), b: mix(s[2]), a: 255 };
        }
        Some(Image::from_rgba8(pb))
    }
}

fn wallpaper_style() -> String {
    let mut v = [0u16; 8];
    let mut sz = (v.len() * 2) as u32;
    unsafe {
        let _ = RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Control Panel\\Desktop"),
            w!("WallpaperStyle"),
            RRF_RT_REG_SZ,
            None,
            Some(v.as_mut_ptr() as _),
            Some(&mut sz),
        );
    }
    let len = v.iter().position(|&c| c == 0).unwrap_or(0);
    String::from_utf16_lossy(&v[..len])
}

/// In-place box blur of an RGBA buffer, radius r, horizontal then vertical.
fn box_blur(buf: &mut [u8], w: usize, h: usize, r: usize) {
    let mut tmp = buf.to_vec();
    for pass in 0..2 {
        let (len, lines) = if pass == 0 { (w, h) } else { (h, w) };
        let idx = |line: usize, i: usize| if pass == 0 { (line * w + i) * 4 } else { (i * w + line) * 4 };
        for line in 0..lines {
            for i in 0..len {
                let a = i.saturating_sub(r);
                let b = (i + r).min(len - 1);
                let mut acc = [0u32; 4];
                for j in a..=b {
                    let p = idx(line, j);
                    for c in 0..4 {
                        acc[c] += buf[p + c] as u32;
                    }
                }
                let n = (b - a + 1) as u32;
                let p = idx(line, i);
                for c in 0..4 {
                    tmp[p + c] = (acc[c] / n) as u8;
                }
            }
        }
        buf.copy_from_slice(&tmp);
    }
}

pub fn work_area() -> RECT {
    let mut r = RECT::default();
    unsafe {
        let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut r as *mut _ as _), Default::default());
    }
    r
}

// ---------- icons and launch ----------

/// Shell icon of a file/shortcut as straight-alpha RGBA image.
/// Shell icon as RGBA pixels. Thread-safe (needs COM on the calling thread): icons.rs runs it on workers.
pub fn icon_pixels(path: &Path, px: i32) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    unsafe {
        let p = HSTRING::from(path.as_os_str());
        let f: IShellItemImageFactory = SHCreateItemFromParsingName(&p, None).ok()?;
        let hb = f.GetImage(SIZE { cx: px, cy: px }, SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK).ok()?;
        let img = bitmap_to_pixels(hb);
        let _ = DeleteObject(hb.into());
        img
    }
}

unsafe fn bitmap_to_pixels(hb: HBITMAP) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let mut bm = BITMAP::default();
    if GetObjectW(hb.into(), std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as _)) == 0 {
        return None;
    }
    let (w, h) = (bm.bmWidth, bm.bmHeight.abs());
    if w <= 0 || h <= 0 {
        return None;
    }
    let mut bi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h, // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let dc = GetDC(None);
    let lines = GetDIBits(dc, hb, 0, h as u32, Some(buf.as_mut_ptr() as _), &mut bi, DIB_RGB_COLORS);
    ReleaseDC(None, dc);
    if lines == 0 {
        return None;
    }
    // Old icons may have no alpha at all -> make opaque.
    let has_alpha = buf.chunks_exact(4).any(|p| p[3] != 0);
    for p in buf.chunks_exact_mut(4) {
        p.swap(0, 2); // BGRA -> RGBA
        if !has_alpha {
            p[3] = 255;
        }
    }
    let mut pb = SharedPixelBuffer::<Rgba8Pixel>::new(w as u32, h as u32);
    pb.make_mut_bytes().copy_from_slice(&buf);
    Some(pb)
}

pub fn open(path: &Path) {
    unsafe {
        let p = HSTRING::from(path.as_os_str());
        ShellExecuteW(None, w!("open"), &p, PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}