// Кратко: всё, что про Windows — стекло окна, иконки ярлыков, запуск, путь к рабочему столу.

use std::cell::Cell;
use std::path::{Path, PathBuf};

use slint::{Image, Rgba8Pixel, SharedPixelBuffer};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{GENERIC_READ, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RRF_RT_REG_SZ};
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

/// COM + OLE on the UI thread (STA). OLE = RegisterDragDrop works (files dragged in from Explorer).
pub fn com_init() {
    unsafe {
        if windows::Win32::System::Ole::OleInitialize(None).is_err() {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
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

/// Frameless popup with per-pixel alpha and no system backdrop (name hint): DWM blends the DIB's
/// alpha, nothing drawn behind it.
pub fn clear(hwnd: HWND) {
    unsafe {
        let off: i32 = 1;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_TRANSITIONS_FORCEDISABLED, &off as *const _ as _, 4);
        let c = DWMWCP_DONOTROUND;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &c as *const _ as _, 4);
        let m = MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 };
        let _ = DwmExtendFrameIntoClientArea(hwnd, &m);
        let b = DWMSBT_NONE;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &b as *const _ as _, 4);
        let none: u32 = 0xFFFF_FFFE; // DWMWA_COLOR_NONE: no caption band if it ever gets a frame
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_CAPTION_COLOR, &none as *const _ as _, 4);
    }
}

/// Native popup menu at the cursor, owned by `owner`. Items: (id, text, enabled); id 0 = separator.
/// Returns the picked id, 0 = dismissed. Menu colors follow the app theme (uxtheme SetPreferredAppMode).
pub fn popup_menu(owner: HWND, items: &[(u32, String, bool)], dark: bool) -> u32 {
    let checked_items: Vec<(u32, String, bool, bool)> =
        items.iter().map(|(id, t, on)| (*id, t.clone(), *on, false)).collect();
    popup_menu_checked(owner, &checked_items, dark)
}

/// Native popup menu at the cursor, owned by `owner`. Items: (id, text, enabled, checked); id 0 = separator.
/// Returns the picked id, 0 = dismissed. Menu colors follow the app theme (uxtheme SetPreferredAppMode).
pub fn popup_menu_checked(owner: HWND, items: &[(u32, String, bool, bool)], dark: bool) -> u32 {
    unsafe {
        // undocumented but stable since 1903: ordinal 135 SetPreferredAppMode(2 dark / 3 light), 136 FlushMenuThemes
        if let Ok(ux) = LoadLibraryW(w!("uxtheme.dll")) {
            if let Some(f) = GetProcAddress(ux, windows::core::PCSTR(135 as *const u8)) {
                let set: extern "system" fn(i32) -> i32 = std::mem::transmute(f);
                set(if dark { 2 } else { 3 });
            }
            if let Some(f) = GetProcAddress(ux, windows::core::PCSTR(136 as *const u8)) {
                let flush: extern "system" fn() = std::mem::transmute(f);
                flush();
            }
        }
        let Ok(m) = CreatePopupMenu() else { return 0 };
        for (id, text, on, checked) in items {
            if *id == 0 {
                let _ = AppendMenuW(m, MF_SEPARATOR, 0, PCWSTR::null());
            } else {
                let t = HSTRING::from(text.as_str());
                let mut f = if *on { MF_STRING } else { MF_STRING | MF_GRAYED };
                if *checked {
                    f |= MF_CHECKED;
                }
                let _ = AppendMenuW(m, f, *id as usize, &t);
            }
        }
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        // owner must be foreground, else the menu does not close on a click elsewhere
        let _ = SetForegroundWindow(owner);
        let r = TrackPopupMenuEx(m, (TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY).0, p.x, p.y, owner, None);
        let _ = PostMessageW(Some(owner), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(m);
        r.0 as u32
    }
}

// ---------- system tray icon ----------

/// Generate a crisp 32x32 RGBA glass-folder icon (translucent cyan-blue folder + bright frosted border + 2x2 mini-tiles).
pub fn tray_icon_rgba() -> (u32, u32, Vec<u8>) {
    const W: usize = 32;
    const H: usize = 32;
    let mut px = vec![0u8; W * H * 4];

    let round_box_sdf = |x: f32, y: f32, cx: f32, cy: f32, hw: f32, hh: f32, r: f32| -> f32 {
        let dx = (x - cx).abs() - (hw - r);
        let dy = (y - cy).abs() - (hh - r);
        let ox = dx.max(0.0);
        let oy = dy.max(0.0);
        (ox * ox + oy * oy).sqrt() + dx.max(dy).min(0.0) - r
    };

    let blend = |dst: &mut [u8], r: f32, g: f32, b: f32, a: f32| {
        let sa = a.clamp(0.0, 1.0);
        if sa <= 0.001 {
            return;
        }
        let da = dst[3] as f32 / 255.0;
        let out_a = sa + da * (1.0 - sa);
        if out_a <= 0.001 {
            return;
        }
        let mix = |sc: f32, dc: u8| -> u8 {
            ((sc * sa + (dc as f32 / 255.0) * da * (1.0 - sa)) / out_a * 255.0)
                .round()
                .clamp(0.0, 255.0) as u8
        };
        dst[0] = mix(r, dst[0]);
        dst[1] = mix(g, dst[1]);
        dst[2] = mix(b, dst[2]);
        dst[3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
    };

    for py in 0..H {
        for px_x in 0..W {
            let x = px_x as f32 + 0.5;
            let y = py as f32 + 0.5;
            // Folder silhouette = union of top-left tab and rounded body
            let d_body = round_box_sdf(x, y, 16.0, 18.0, 13.0, 9.5, 3.2);
            let d_tab = round_box_sdf(x, y, 9.5, 9.0, 6.2, 4.0, 2.2);
            let d_folder = d_body.min(d_tab);

            let idx = (py * W + px_x) * 4;
            let cell = &mut px[idx..idx + 4];

            // Outer translucent glass fill
            let fill_cov = (0.75 - d_folder).clamp(0.0, 1.0);
            if fill_cov > 0.0 {
                let t = ((y - 6.0) / 22.0).clamp(0.0, 1.0);
                let fr = 0.44 - 0.16 * t;
                let fg = 0.80 - 0.18 * t;
                let fb = 1.00 - 0.06 * t;
                blend(cell, fr, fg, fb, fill_cov * 0.86);
            }

            // Bright frosted border (visible on both dark and light taskbars)
            let border_dist = (d_folder + 0.55).abs();
            let border_cov = (1.0 - border_dist / 1.15).clamp(0.0, 1.0);
            if border_cov > 0.0 {
                blend(cell, 0.94, 0.98, 1.0, border_cov * 0.95);
            }

            // 2x2 mini-tile grid inside the glass folder body
            for &(tx, ty) in &[(11.5, 15.0), (20.5, 15.0), (11.5, 21.5), (20.5, 21.5)] {
                let dt = round_box_sdf(x, y, tx, ty, 3.0, 2.2, 0.9);
                let tc = (0.6 - dt).clamp(0.0, 1.0);
                if tc > 0.0 {
                    blend(cell, 0.96, 0.99, 1.0, tc * 0.90);
                }
            }
        }
    }
    (W as u32, H as u32, px)
}

/// Localized tray menu items: (id, label, enabled, checked). `id == 0` is a separator.
pub fn tray_menu_items(en: bool, autostart: bool) -> Vec<(u32, String, bool, bool)> {
    vec![
        (1, if en { "Start with Windows" } else { "Запуск с Windows" }.into(), true, autostart),
        (2, if en { "Refresh folders" } else { "Обновить папки" }.into(), true, false),
        (0, String::new(), true, false),
        (3, if en { "Quit" } else { "Выход" }.into(), true, false),
    ]
}

const WM_TRAY: u32 = WM_APP + 20;
const TRAY_ID: u32 = 1;
const TRAY_CLASS: PCWSTR = w!("GlassFoldersTray");

struct TrayState {
    taskbar_msg: u32,
    in_menu: Cell<bool>,
    on_open: Box<dyn Fn(HWND)>,
    nid: NOTIFYICONDATAW,
    active: Cell<bool>,
}

/// Native Win32 system tray icon with TaskbarCreated recovery and clean teardown.
pub struct Tray {
    hwnd: HWND,
    hicon: HICON,
    state: *mut TrayState,
}

impl Tray {
    pub fn new(on_open: impl Fn(HWND) + 'static) -> Option<Self> {
        if crate::plat::offscreen() {
            return None;
        }
        let (w, h, rgba) = tray_icon_rgba();
        let hicon = create_rgba_icon(w, h, &rgba)?;
        let inst = unsafe { GetModuleHandleW(None) }.ok()?;
        unsafe {
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(tray_wndproc),
                hInstance: inst.into(),
                lpszClassName: TRAY_CLASS,
                ..Default::default()
            };
            RegisterClassExW(&wc);
        }
        // Top-level hidden window (NOT HWND_MESSAGE) so RegisterWindowMessageW("TaskbarCreated") reaches it.
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                TRAY_CLASS,
                w!("Glass Folders Tray"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(inst.into()),
                None,
            )
        }
        .ok()?;
        let taskbar_msg = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
        let mut nid = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: TRAY_ID,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: WM_TRAY,
            hIcon: hicon,
            ..Default::default()
        };
        for (dst, ch) in nid.szTip.iter_mut().zip("Glass Folders".encode_utf16()) {
            *dst = ch;
        }
        let state = Box::into_raw(Box::new(TrayState {
            taskbar_msg,
            in_menu: Cell::new(false),
            on_open: Box::new(on_open),
            nid,
            active: Cell::new(true),
        }));
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);
            let _ = Shell_NotifyIconW(NIM_ADD, &(*state).nid);
        }
        Some(Tray { hwnd, hicon, state })
    }

    pub fn remove_icon(&self) {
        unsafe {
            let st = &*self.state;
            if st.active.replace(false) {
                let _ = Shell_NotifyIconW(NIM_DELETE, &st.nid);
            }
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        self.remove_icon();
        unsafe {
            SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
            let _ = DestroyWindow(self.hwnd);
            drop(Box::from_raw(self.state));
            let _ = DestroyIcon(self.hicon);
        }
    }
}

fn create_rgba_icon(w: u32, h: u32, rgba: &[u8]) -> Option<HICON> {
    let mut bgra = Vec::with_capacity(rgba.len());
    for c in rgba.chunks_exact(4) {
        bgra.extend_from_slice(&[c[2], c[1], c[0], c[3]]);
    }
    let mask_bytes = vec![0u8; ((w as usize + 31) / 32 * 4) * h as usize];
    unsafe {
        let hbm_color = CreateBitmap(w as i32, h as i32, 1, 32, Some(bgra.as_ptr() as _));
        let hbm_mask = CreateBitmap(w as i32, h as i32, 1, 1, Some(mask_bytes.as_ptr() as _));
        let ii = ICONINFO {
            fIcon: true.into(),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: hbm_mask,
            hbmColor: hbm_color,
        };
        let icon = CreateIconIndirect(&ii).ok();
        let _ = DeleteObject(hbm_color.into());
        let _ = DeleteObject(hbm_mask.into());
        icon
    }
}

unsafe extern "system" fn tray_wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const TrayState;
    if !ptr.is_null() {
        let st = &*ptr;
        if msg == WM_TRAY {
            let ev = (lp.0 as u32) & 0xFFFF;
            if matches!(ev, WM_RBUTTONUP | WM_LBUTTONUP | WM_CONTEXTMENU) && !st.in_menu.replace(true) {
                (st.on_open)(hwnd);
                st.in_menu.set(false);
            }
            return LRESULT(0);
        }
        if st.taskbar_msg != 0 && msg == st.taskbar_msg && st.active.get() {
            let _ = Shell_NotifyIconW(NIM_ADD, &st.nid);
            return LRESULT(0);
        }
    }
    DefWindowProcW(hwnd, msg, wp, lp)
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