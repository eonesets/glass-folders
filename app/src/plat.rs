// Кратко: свой «мост» между Slint и окнами Windows. Slint рисует картинку в память,
// мы сами показываем её в окне с прозрачностью (чтобы было видно стекло) и передаём мышь/клавиши.
//
// Why: Slint winit+software path presents via softbuffer -> no per-pixel alpha -> glass hidden.
// Here: SoftwareRenderer -> premultiplied BGRA DIB section -> BitBlt into a window whose DWM
// frame is extended into the whole client area, so alpha is honored and the backdrop shows.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use slint::platform::software_renderer::{PremultipliedRgbaColor, RepaintBufferType, SoftwareRenderer, TargetPixel};
use slint::platform::{
    EventLoopProxy, Key, Platform, PointerEventButton, Renderer, WindowAdapter, WindowEvent, WindowProperties,
};
use slint::{EventLoopError, LogicalPosition, LogicalSize, PhysicalPosition, PhysicalSize, PlatformError};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::HiDpi::{GetDpiForWindow, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VIRTUAL_KEY, VK_BACK, VK_CONTROL,
    VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::*;

// ---------- dev perf log: env GF_PERF=file -> timings (paint ms, open steps) ----------

thread_local! {
    static PERF: Option<RefCell<std::fs::File>> =
        std::env::var("GF_PERF").ok().and_then(|p| std::fs::File::create(p).ok()).map(RefCell::new);
    static T0: Instant = Instant::now();
}

/// One line "t_ms text" to the GF_PERF file (no-op without it).
pub fn perf(msg: impl FnOnce() -> String) {
    PERF.with(|p| {
        if let Some(f) = p {
            use std::io::Write;
            let t = T0.with(|t| t.elapsed().as_secs_f64() * 1000.0);
            let _ = writeln!(f.borrow_mut(), "{t:9.1} {}", msg());
        }
    });
}

// ---------- pixel type = DIB layout (premultiplied BGRA) ----------

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Bgra {
    pub(crate) b: u8,
    pub(crate) g: u8,
    pub(crate) r: u8,
    pub(crate) a: u8,
}

impl TargetPixel for Bgra {
    fn blend(&mut self, c: PremultipliedRgbaColor) {
        let inv = 255 - c.alpha as u16;
        let mix = |src: u8, dst: u8| (src as u16 + (dst as u16 * inv + 127) / 255) as u8;
        self.r = mix(c.red, self.r);
        self.g = mix(c.green, self.g);
        self.b = mix(c.blue, self.b);
        self.a = mix(c.alpha, self.a);
    }
    fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        Bgra { b, g, r, a: 255 }
    }
    fn background() -> Self {
        Bgra::default() // fully transparent: glass shows through
    }
}

// ---------- window ----------

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Panel,
    Tile,
    /// name hint near the cursor: topmost, no focus, clicks go through
    Hint,
}

/// Z-order policy for desktop-bound windows (tiles and pinned panels).
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ZPlace {
    /// Normal desktop mode: stay at the bottom of Z-order.
    Bottom,
    /// Show Desktop (Win+D) active: stay right above the raised desktop host.
    AboveHost,
}

/// Z-order target for a window. Tiles and pinned panels are desktop-bound; unpinned panels and hints are unmanaged.
pub fn z_place(kind: Kind, pinned: bool, show_desktop: bool) -> Option<ZPlace> {
    if kind == Kind::Tile || (kind == Kind::Panel && pinned) {
        Some(if show_desktop { ZPlace::AboveHost } else { ZPlace::Bottom })
    } else {
        None
    }
}

/// Relative Z-order layer among desktop-bound windows: pinned panels stay above tiles.
pub fn z_layer(kind: Kind, pinned: bool) -> Option<u8> {
    match (kind, pinned) {
        (Kind::Tile, _) => Some(1),
        (Kind::Panel, true) => Some(2),
        _ => None,
    }
}

/// Z-order action to apply when the foreground window changes.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum LayerAction {
    RaiseAboveHost,
    RestoreBottom,
}

/// Decides whether desktop-bound windows should be raised above the desktop host or restored to bottom.
pub fn on_foreground_change(was_show_desktop: bool, is_desktop_fg: bool) -> Option<LayerAction> {
    match (was_show_desktop, is_desktop_fg) {
        (_, true) => Some(LayerAction::RaiseAboveHost),
        (true, false) => Some(LayerAction::RestoreBottom),
        (false, false) => None,
    }
}

/// Tracks whether the cached Slint snapshot during zview needs a fresh SoftwareRenderer::render() pass.
#[derive(Default)]
pub(crate) struct ZoomSnap {
    rendered: bool,
    dirty: bool,
}

impl ZoomSnap {
    /// Returns true on the first frame after Default/clear() and after mark_dirty(), then false until dirty again.
    pub(crate) fn frame(&mut self) -> bool {
        if !self.rendered || self.dirty {
            self.rendered = true;
            self.dirty = false;
            true
        } else {
            false
        }
    }

    pub(crate) fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub(crate) fn clear(&mut self) {
        self.rendered = false;
        self.dirty = false;
    }
}

struct Dib {
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut Bgra,
    w: u32,
    h: u32,
}

impl Drop for Dib {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bmp.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

pub struct GfWindow {
    window: slint::Window,
    renderer: SoftwareRenderer,
    hwnd: Cell<HWND>,
    size: Cell<PhysicalSize>,
    scale: Cell<f32>, // = dpi * zoom
    dpi: Cell<f32>,
    zoom: Cell<f32>, // icon size setting
    kind: Cell<Kind>,
    pinned: Cell<bool>, // panel pinned on the desktop: bottom z-order, glass stays live
    tracking: Cell<bool>,
    dib: RefCell<Option<Dib>>,
    on_deactivate: RefCell<Option<Rc<dyn Fn()>>>,
    drag: Cell<Option<Drag>>,
    on_moved: RefCell<Option<Rc<dyn Fn(RECT)>>>,
    surrogate: Cell<u16>, // WM_CHAR high surrogate waiting for its pair
    /// Open / close animation: whole content scaled into this screen rect, faded (0..1).
    /// Slint keeps its full size (no relayout per frame); paint + mouse are scaled.
    zview: Cell<Option<(RECT, f32)>>,
    zoom_snap: RefCell<ZoomSnap>,
    last_zoom_rect: Cell<Option<RECT>>,
    zdib: RefCell<Option<Dib>>, // scaled frame
    glide: RefCell<Option<slint::Timer>>,
    /// files dragged in from Explorer: app callback (see enable_drop); drop_reg = RegisterDragDrop done
    on_drop: RefCell<Option<DropFn>>,
    drop_reg: Cell<bool>,
}

impl Drop for GfWindow {
    fn drop(&mut self) {
        // window gone for good (removed tile, closed pinned panel): native window + drop target too
        let hwnd = self.hwnd.get();
        WINDOWS.with(|m| m.borrow_mut().remove(&(hwnd.0 as isize)));
        unsafe {
            if self.drop_reg.get() {
                let _ = windows::Win32::System::Ole::RevokeDragDrop(hwnd);
            }
            let _ = DestroyWindow(hwnd);
        }
    }
}

/// Window drag in progress (tile, or panel header): move (anywhere) or resize (bottom-right grip). Screen coords.
#[derive(Clone, Copy)]
struct Drag {
    resize: bool,
    cursor: POINT,
    rect: RECT,
    moved: bool,
}

/// Bottom-right square of a tile that resizes instead of moving (logical px).
pub const GRIP: f32 = 16.0;
// Tile grid (logical px): one icon cell. Tile size = cols x rows cells + padding; Tile in app.slint uses the same numbers.
pub const CELL_W: f32 = 48.0;
pub const CELL_H: f32 = 42.0;
const PAD_W: f32 = 12.0; // tile width  = cols * CELL_W + PAD_W
const PAD_H: f32 = 30.0; // tile height = rows * CELL_H + PAD_H (name strip)
const MIN_CELLS: i32 = 2;

/// Tile row pitch + vertical padding: with name strip (42, 30), or without (48, 12 = square cells, even edges).
fn tile_v() -> (f32, f32) {
    if TILE_NAMED.with(|v| v.get()) { (CELL_H, PAD_H) } else { (CELL_W, PAD_W) }
}

/// Tile size in cells (cols, rows) for a size in physical px, min 2x2.
pub fn tile_cells(w: i32, h: i32, s: f32) -> (i32, i32) {
    let (ch, ph) = tile_v();
    let cols = (((w as f32 / s - PAD_W) / CELL_W).round() as i32).max(MIN_CELLS);
    let rows = (((h as f32 / s - ph) / ch).round() as i32).max(MIN_CELLS);
    (cols, rows)
}

/// Physical px size of a tile of cols x rows cells.
pub fn tile_size(cols: i32, rows: i32, s: f32) -> (i32, i32) {
    let (ch, ph) = tile_v();
    (((cols as f32 * CELL_W + PAD_W) * s).round() as i32, ((rows as f32 * ch + ph) * s).round() as i32)
}

/// Snap a tile size (physical px) to whole cells, min 2x2.
pub fn snap_size(w: i32, h: i32, s: f32) -> (i32, i32) {
    let (c, r) = tile_cells(w, h, s);
    tile_size(c, r, s)
}

/// Tiles show the folder name (taller cells) or not (square cells).
pub fn set_tile_named(on: bool) {
    TILE_NAMED.with(|v| v.set(on));
}

/// Snap a tile position (physical px) to the desktop grid: cell pitch, origin = work area corner.
pub fn snap_pos(x: i32, y: i32, s: f32) -> (i32, i32) {
    let wa = crate::win::work_area();
    let snap = |v: i32, o: i32, pitch: f32| o + (((v - o) as f32 / (pitch * s)).round() * pitch * s).round() as i32;
    (snap(x, wa.left, CELL_W), snap(y, wa.top, CELL_H))
}
const DRAG_START: i32 = 4; // px before press becomes drag (else it is a click)
/// Interval for timers that step an animation: "every frame". The event loop runs one pass per screen
/// refresh while such a timer is due (DwmFlush), so each pass = one new frame, evenly spaced.
pub const FRAME: Duration = Duration::from_millis(1);
const GLIDE_MS: f32 = 130.0; // tile settles onto the grid after release

thread_local! {
    /// Tiles land on the grid after a move (setting). Size always snaps to whole cells.
    static SNAP: Cell<bool> = const { Cell::new(true) };
    static TILE_NAMED: Cell<bool> = const { Cell::new(true) };
    static WINDOWS: RefCell<HashMap<isize, Weak<GfWindow>>> = RefCell::new(HashMap::new());
    static ALL: RefCell<Vec<Weak<GfWindow>>> = const { RefCell::new(Vec::new()) };
    static SCALE_XS: RefCell<Vec<(usize, usize, u32)>> = const { RefCell::new(Vec::new()) };
    static ZVIEW_RENDERS: Cell<usize> = const { Cell::new(0) };
    static SHOW_DESKTOP: Cell<bool> = const { Cell::new(false) };
    static REORDERING: Cell<bool> = const { Cell::new(false) };
}

pub fn reset_zview_renders() {
    ZVIEW_RENDERS.with(|c| c.set(0));
}

pub fn zview_renders() -> usize {
    ZVIEW_RENDERS.with(|c| c.get())
}

pub fn last_zoom_rect(window: &slint::Window) -> Option<RECT> {
    find(window).and_then(|g| g.last_zoom_rect.get())
}

fn attach_desktop_owner(hwnd: HWND, attach: bool) {
    if offscreen() {
        return;
    }
    let owner = if attach {
        crate::desk::desktop_host().map(|h| h.0 as isize).unwrap_or(0)
    } else {
        0
    };
    unsafe {
        let cur = GetWindowLongPtrW(hwnd, GWLP_HWNDPARENT);
        if cur != owner {
            SetWindowLongPtrW(hwnd, GWLP_HWNDPARENT, owner);
        }
    }
}

fn is_desktop_hwnd(hwnd: HWND) -> bool {
    if hwnd.is_invalid() {
        return false;
    }
    unsafe {
        let root = GetAncestor(hwnd, GA_ROOT);
        let target = if root.is_invalid() { hwnd } else { root };
        let mut buf = [0u16; 32];
        let n = GetClassNameW(target, &mut buf) as usize;
        let class = String::from_utf16_lossy(&buf[..n]);
        crate::desk::is_desktop_class(&class)
    }
}

fn managed_windows() -> Vec<(u8, Rc<GfWindow>)> {
    ALL.with(|v| {
        v.borrow()
            .iter()
            .filter_map(|w| w.upgrade())
            .filter_map(|g| z_layer(g.kind.get(), g.pinned.get()).map(|layer| (layer, g)))
            .collect()
    })
}

/// Re-attach desktop owner (in case Explorer moved SHELLDLL_DefView between Progman and WorkerW)
/// and keep visible tiles & pinned panels right above the desktop host (pinned above tiles).
fn sync_desktop_layer() {
    if offscreen() {
        return;
    }
    let Some(host) = crate::desk::desktop_host() else { return };
    let mut managed = managed_windows();
    for (_, g) in &managed {
        attach_desktop_owner(g.hwnd.get(), true);
    }
    managed.sort_by_key(|(layer, _)| *layer);
    unsafe {
        // Find the lowest non-managed window immediately above host so we stay above host
        // without jumping over normal application windows when the user merely clicked the desktop.
        let mut prev = GetWindow(host, GW_HWNDPREV).unwrap_or_default();
        while !prev.is_invalid()
            && lookup(prev).is_some_and(|g| z_layer(g.kind.get(), g.pinned.get()).is_some())
        {
            prev = GetWindow(prev, GW_HWNDPREV).unwrap_or_default();
        }
        let anchor = if prev.is_invalid() { HWND_TOP } else { prev };
        REORDERING.with(|r| r.set(true));
        for (_, g) in &managed {
            let hwnd = g.hwnd.get();
            if IsWindowVisible(hwnd).as_bool() {
                let _ = SetWindowPos(hwnd, Some(anchor), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
        }
        REORDERING.with(|r| r.set(false));
    }
}

/// Restore visible pinned panels to HWND_BOTTOM first, then visible tiles, so pinned panels stay above tiles
/// and both sit behind normal/background app windows after leaving Show Desktop.
fn restore_desktop_bottom() {
    if offscreen() {
        return;
    }
    let mut managed = managed_windows();
    for (_, g) in &managed {
        attach_desktop_owner(g.hwnd.get(), true);
    }
    // Pinned panels (layer 2) go to HWND_BOTTOM first, then tiles (layer 1) go beneath them.
    managed.sort_by_key(|(layer, _)| std::cmp::Reverse(*layer));
    unsafe {
        REORDERING.with(|r| r.set(true));
        for (_, g) in &managed {
            let hwnd = g.hwnd.get();
            if IsWindowVisible(hwnd).as_bool() {
                let _ = SetWindowPos(hwnd, Some(HWND_BOTTOM), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
        }
        REORDERING.with(|r| r.set(false));
    }
}

unsafe extern "system" fn fg_hook(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _id_event_thread: u32,
    _time: u32,
) {
    if lookup(hwnd).is_some_and(|g| g.pinned.get()) {
        return;
    }
    let sd = is_desktop_hwnd(hwnd);
    let was = SHOW_DESKTOP.with(|v| v.replace(sd));
    match on_foreground_change(was, sd) {
        Some(LayerAction::RaiseAboveHost) => sync_desktop_layer(),
        Some(LayerAction::RestoreBottom) => restore_desktop_bottom(),
        None => {}
    }
}

fn lookup(hwnd: HWND) -> Option<Rc<GfWindow>> {
    WINDOWS.with(|m| m.borrow().get(&(hwnd.0 as isize)).and_then(|w| w.upgrade()))
}

/// Find our adapter for a Slint window.
fn find(window: &slint::Window) -> Option<Rc<GfWindow>> {
    ALL.with(|v| v.borrow().iter().filter_map(|w| w.upgrade()).find(|g| std::ptr::eq(&g.window, window)))
}

pub fn hwnd_of(window: &slint::Window) -> Option<HWND> {
    find(window).map(|g| g.hwnd.get())
}

/// Tile = never takes focus, stays at the bottom of z-order.
pub fn set_kind(window: &slint::Window, kind: Kind) {
    if let Some(g) = find(window) {
        g.kind.set(kind);
        let hwnd = g.hwnd.get();
        attach_desktop_owner(hwnd, kind == Kind::Tile || g.pinned.get());
        unsafe {
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            let n = if kind != Kind::Panel || offscreen() { ex | WS_EX_NOACTIVATE.0 as isize } else { ex & !(WS_EX_NOACTIVATE.0 as isize) };
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, n);
            if kind == Kind::Hint {
                // Hint: plain popup. A thick frame makes Windows keep the window >= ~39px tall whatever
                // WM_GETMINMAXINFO says -> the 28px label got a second, empty glass strip under it.
                // No frame = no system backdrop: the label paints its own (near-opaque) background.
                let st = GetWindowLongPtrW(hwnd, GWL_STYLE) & !((WS_CAPTION.0 | WS_THICKFRAME.0) as isize);
                SetWindowLongPtrW(hwnd, GWL_STYLE, st);
                let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED);
            }
        }
    }
}

/// Icon size: window content zoom on top of DPI. Slint sees it as the scale factor.
pub fn set_zoom(window: &slint::Window, zoom: f32) {
    if let Some(g) = find(window) {
        g.zoom.set(zoom);
        let s = g.dpi.get() * zoom;
        g.scale.set(s);
        g.window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: s });
    }
}

/// Pinned panel: lives on the desktop like a tile (bottom z-order, live glass when not focused),
/// still takes focus when clicked (rename, keys).
pub fn set_pinned(window: &slint::Window, on: bool) {
    let Some(g) = find(window) else { return };
    g.pinned.set(on);
    let hwnd = g.hwnd.get();
    attach_desktop_owner(hwnd, on || g.kind.get() == Kind::Tile);
    if on {
        unsafe {
            if SHOW_DESKTOP.with(|v| v.get()) {
                sync_desktop_layer();
            } else {
                let _ = SetWindowPos(hwnd, Some(HWND_BOTTOM), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                let _ = PostMessageW(Some(hwnd), WM_TILES_DOWN, WPARAM(0), LPARAM(0));
            }
            SendMessageW(hwnd, WM_NCACTIVATE, Some(WPARAM(1)), Some(LPARAM(0)));
        }
    }
}

/// Called after a tile was moved/resized by the user, with its new screen rect.
pub fn set_on_moved(window: &slint::Window, f: impl Fn(RECT) + 'static) {
    if let Some(g) = find(window) {
        *g.on_moved.borrow_mut() = Some(Rc::new(f));
    }
}

/// Panel: header was pressed -> following mouse moves drag the window (same code as tile move).
pub fn start_move(window: &slint::Window) {
    let Some(g) = find(window) else { return };
    let mut d = Drag { resize: false, cursor: cursor_pos(), rect: RECT::default(), moved: false };
    unsafe {
        let _ = GetWindowRect(g.hwnd.get(), &mut d.rect);
    }
    g.drag.set(Some(d));
}

pub fn set_snap(on: bool) {
    SNAP.with(|s| s.set(on));
}

/// Animation frame: the whole content scaled into screen rect `r` (aspect preserved, centered),
/// faded by `fade` 0..1. Content size is untouched (no relayout per frame); Slint renders once
/// and only re-renders if the window is marked dirty (e.g. icons arrived).
pub fn set_zoom_view(window: &slint::Window, r: RECT, fade: f32) {
    let Some(g) = find(window) else { return };
    if g.zview.get().is_none() {
        g.zoom_snap.borrow_mut().clear();
    }
    g.zview.set(Some((r, fade.clamp(0.0, 1.0))));
    g.last_zoom_rect.set(Some(r));
    let hwnd = g.hwnd.get();
    let (w, h) = ((r.right - r.left).max(1), (r.bottom - r.top).max(1));
    let PhysicalSize { width: sw, height: sh } = g.size.get();
    unsafe {
        // frame ready before the window changes, then move + blit in one go (no WM_PAINT round trip,
        // no copy of old bits): DWM gets new geometry and new content together
        let sdc = GetDC(None);
        let need_draw = g.zoom_snap.borrow_mut().frame()
            || g.dib.borrow().as_ref().map(|d| (d.w, d.h)) != Some((sw, sh));
        let drew = if need_draw { g.draw(sdc) } else { true };
        let ok = drew && g.zoom_into(sdc, w as u32, h as u32);
        let _ = ReleaseDC(None, sdc);
        // DWM may show one frame between move and blit: shrinking -> new (smaller) picture first, growing ->
        // bigger window first. Either way that frame has a bit of empty glass, never cut-off content.
        let mut cur = RECT::default();
        let _ = GetWindowRect(hwnd, &mut cur);
        let shrink = w < cur.right - cur.left;
        if ok && shrink {
            g.blit();
        }
        let _ = SetWindowPos(hwnd, None, r.left, r.top, w, h, SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOCOPYBITS | SWP_NOREDRAW);
        if ok {
            if !shrink {
                g.blit();
            }
            let _ = ValidateRect(Some(hwnd), None);
        } else {
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
    }
}

/// Animation over: window = content at full size again, top-left at `home` (None = stay, e.g. hidden).
pub fn end_zoom_view(window: &slint::Window, home: Option<POINT>) {
    let Some(g) = find(window) else { return };
    if g.zview.take().is_none() {
        return;
    }
    g.zoom_snap.borrow_mut().clear();
    g.zdib.borrow_mut().take();
    if let Some(p) = home {
        let PhysicalSize { width, height } = g.size.get();
        let hwnd = g.hwnd.get();
        unsafe {
            let _ = SetWindowPos(hwnd, None, p.x, p.y, width as i32, height as i32, SWP_NOZORDER | SWP_NOACTIVATE);
            let _ = InvalidateRect(Some(hwnd), None, false);
            let _ = UpdateWindow(hwnd);
        }
    }
}

/// Open / close animation frame being shown (window is not at its full size)?
pub fn zoom_view_on(window: &slint::Window) -> bool {
    find(window).is_some_and(|g| g.zview.get().is_some())
}

/// Aspect-preserving (contain, centered) bilinear scale of premultiplied pixels, times `fade` (0..1).
/// Every pixel in `dst` is written (transparent outside content rect). Reuses thread-local scratch buffer.
pub(crate) fn scale_pixels(src: &[Bgra], sw: usize, sh: usize, dst: &mut [Bgra], dw: usize, dh: usize, fade: f32) {
    if dw == 0 || dh == 0 {
        return;
    }
    let fade = fade.clamp(0.0, 1.0);
    let f = (fade * 256.0).round() as u32;
    if sw == 0 || sh == 0 || f == 0 {
        dst[..dw * dh].fill(Bgra::default());
        return;
    }
    let scale = (dw as f32 / sw as f32).min(dh as f32 / sh as f32);
    let cw = ((sw as f32 * scale).round() as usize).clamp(1, dw);
    let ch = ((sh as f32 * scale).round() as usize).clamp(1, dh);
    let ox = (dw - cw) / 2;
    let oy = (dh - ch) / 2;

    if oy > 0 {
        dst[..oy * dw].fill(Bgra::default());
    }
    if oy + ch < dh {
        dst[(oy + ch) * dw..dw * dh].fill(Bgra::default());
    }

    SCALE_XS.with(|cell| {
        let mut xs = cell.borrow_mut();
        xs.clear();
        xs.reserve(cw);
        for x in 0..cw {
            let fx = ((x as f32 + 0.5) * sw as f32 / cw as f32 - 0.5).max(0.0);
            let x0 = (fx as usize).min(sw - 1);
            let x1 = (x0 + 1).min(sw - 1);
            xs.push((x0, x1, ((fx - x0 as f32) * 256.0) as u32));
        }
        for y in 0..ch {
            let fy = ((y as f32 + 0.5) * sh as f32 / ch as f32 - 0.5).max(0.0);
            let y0 = (fy as usize).min(sh - 1);
            let y1 = (y0 + 1).min(sh - 1);
            let ty = ((fy - y0 as f32) * 256.0) as u32;
            let (r0, r1) = (&src[y0 * sw..y0 * sw + sw], &src[y1 * sw..y1 * sw + sw]);
            let row = &mut dst[(oy + y) * dw..(oy + y) * dw + dw];
            if ox > 0 {
                row[..ox].fill(Bgra::default());
            }
            if ox + cw < dw {
                row[ox + cw..].fill(Bgra::default());
            }
            for (o, &(x0, x1, tx)) in row[ox..ox + cw].iter_mut().zip(xs.iter()) {
                let (a, b, c, d) = (r0[x0], r0[x1], r1[x0], r1[x1]);
                let mix = |p: u8, q: u8, s: u8, t: u8| -> u8 {
                    let top = p as u32 * (256 - tx) + q as u32 * tx;
                    let bot = s as u32 * (256 - tx) + t as u32 * tx;
                    let v = (top * (256 - ty) + bot * ty + 32768) >> 16;
                    ((v * f + 128) >> 8) as u8
                };
                *o = Bgra { b: mix(a.b, b.b, c.b, d.b), g: mix(a.g, b.g, c.g, d.g), r: mix(a.r, b.r, c.r, d.r), a: mix(a.a, b.a, c.a, d.a) };
            }
        }
    });
}

/// Screen rect of a window (physical px).
/// Cursor in window content coords (logical px). Drags use this: a pointer position relative to
/// an element that itself moves with the drag drifts.
pub fn cursor_logical(window: &slint::Window) -> Option<(f32, f32)> {
    let g = find(window)?;
    let mut c = cursor_pos();
    unsafe {
        let _ = ScreenToClient(g.hwnd.get(), &mut c);
    }
    let (x, y) = g.unzoom(c.x as f32, c.y as f32);
    let s = g.scale.get();
    Some((x / s, y / s))
}

pub fn rect_of(window: &slint::Window) -> Option<RECT> {
    let g = find(window)?;
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(g.hwnd.get(), &mut r);
    }
    Some(r)
}

/// Free desktop (icon layer / wallpaper) under screen point p, not some app window.
/// Script mode (virtual cursor): always (the real screen is not part of the test).
pub fn desktop_at(p: POINT) -> bool {
    if VCURSOR.with(|v| v.get()).is_some() {
        return true;
    }
    unsafe {
        let w = WindowFromPoint(p);
        if w.is_invalid() {
            return false;
        }
        let root = GetAncestor(w, GA_ROOT);
        let mut buf = [0u16; 32];
        let n = GetClassNameW(root, &mut buf) as usize;
        let class = String::from_utf16_lossy(&buf[..n]);
        crate::desk::is_desktop_class(&class)
    }
}

thread_local! {
    static ON_QUIT: RefCell<Option<Rc<dyn Fn()>>> = RefCell::new(None);
}

/// App-wide clean quit (close request from outside, session end). Called at most once.
pub fn set_on_quit(f: impl Fn() + 'static) {
    ON_QUIT.with(|q| *q.borrow_mut() = Some(Rc::new(f)));
}

fn quit_now() {
    if let Some(f) = ON_QUIT.with(|q| q.borrow_mut().take()) {
        f();
    }
}

/// Bring window to the foreground (panel back after a dialog / rename box).
pub fn activate(window: &slint::Window) {
    if let Some(g) = find(window) {
        unsafe {
            let _ = SetForegroundWindow(g.hwnd.get());
        }
    }
}

pub fn set_on_deactivate(window: &slint::Window, f: impl Fn() + 'static) {
    if let Some(g) = find(window) {
        *g.on_deactivate.borrow_mut() = Some(Rc::new(f));
    }
}

// ---------- files dragged in from Explorer / the desktop (OLE drop target) ----------

/// Drop effect for an external drag: files (CF_HDROP) only; Ctrl = copy, else move; refused = none.
pub fn drop_effect(has_hdrop: bool, ctrl: bool, allowed: bool) -> u32 {
    use windows::Win32::System::Ole::{DROPEFFECT_COPY, DROPEFFECT_MOVE, DROPEFFECT_NONE};
    if !has_hdrop || !allowed {
        DROPEFFECT_NONE.0
    } else if ctrl {
        DROPEFFECT_COPY.0
    } else {
        DROPEFFECT_MOVE.0
    }
}

/// External drag over a window. Coords = window content, logical px.
pub enum DropEv<'a> {
    /// files over (x, y): return true if they may be dropped here (highlight on)
    Over(f32, f32, &'a [PathBuf]),
    /// drag left / cancelled / done: highlight off
    Leave,
    /// released at (x, y); bool = copy (else move). Return true if anything was taken.
    Drop(f32, f32, &'a [PathBuf], bool),
}

type DropFn = Rc<dyn Fn(DropEv) -> bool>;

/// Accept files dragged in from other apps on this window. Headless test: callback only (scripts call
/// drop_at), no OLE registration (invisible click-through windows are no real drop targets).
pub fn enable_drop(window: &slint::Window, f: impl Fn(DropEv) -> bool + 'static) {
    let Some(g) = find(window) else { return };
    *g.on_drop.borrow_mut() = Some(Rc::new(f));
    if offscreen() || g.drop_reg.get() {
        return;
    }
    let t: windows::Win32::System::Ole::IDropTarget = DropTarget::new(g.hwnd.get()).into();
    if unsafe { windows::Win32::System::Ole::RegisterDragDrop(g.hwnd.get(), &t) }.is_ok() {
        g.drop_reg.set(true);
    }
}

/// Same path as a real drop, without OLE (script `extdrop`). False = window unknown / nothing taken.
pub fn drop_at(window: &slint::Window, x: f32, y: f32, files: &[PathBuf], copy: bool) -> bool {
    let Some(f) = find(window).and_then(|g| g.on_drop.borrow().clone()) else { return false };
    let done = f(DropEv::Drop(x, y, files, copy));
    f(DropEv::Leave);
    done
}

fn drop_fn(hwnd: HWND) -> Option<(Rc<GfWindow>, DropFn)> {
    let g = lookup(hwnd)?;
    let f = g.on_drop.borrow().clone()?;
    Some((g, f))
}

/// Files of a CF_HDROP data object (empty = no files: text, links, ...).
fn hdrop_files(data: &windows::Win32::System::Com::IDataObject) -> Vec<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows::Win32::System::Com::{DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL};
    use windows::Win32::System::Ole::{ReleaseStgMedium, CF_HDROP};
    use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
    let fmt = FORMATETC {
        cfFormat: CF_HDROP.0,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    };
    let mut out = Vec::new();
    unsafe {
        let Ok(mut m) = data.GetData(&fmt) else { return out };
        if m.tymed == TYMED_HGLOBAL.0 as u32 {
            let h = HDROP(m.u.hGlobal.0);
            let n = DragQueryFileW(h, u32::MAX, None);
            for i in 0..n {
                let len = DragQueryFileW(h, i, None) as usize;
                let mut buf = vec![0u16; len + 1];
                let got = DragQueryFileW(h, i, Some(&mut buf)) as usize;
                if got > 0 {
                    out.push(PathBuf::from(std::ffi::OsString::from_wide(&buf[..got])));
                }
            }
        }
        ReleaseStgMedium(&mut m);
    }
    out
}

/// OLE drop target of one window. Files are read on enter; the app callback decides (see DropEv).
#[windows::core::implement(windows::Win32::System::Ole::IDropTarget)]
struct DropTarget {
    hwnd: isize,
    files: RefCell<Vec<PathBuf>>,
    /// shell drag image keeps showing over our window
    helper: Option<windows::Win32::UI::Shell::IDropTargetHelper>,
}

impl DropTarget {
    fn new(hwnd: HWND) -> Self {
        use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
        let helper = unsafe { CoCreateInstance(&windows::Win32::UI::Shell::CLSID_DragDropHelper, None, CLSCTX_INPROC_SERVER) }.ok();
        DropTarget { hwnd: hwnd.0 as isize, files: RefCell::default(), helper }
    }

    fn hwnd(&self) -> HWND {
        HWND(self.hwnd as *mut _)
    }

    /// Screen point -> window content coords (logical px; open / close animation scaling undone).
    fn at(&self, g: &GfWindow, pt: &windows::Win32::Foundation::POINTL) -> (f32, f32) {
        let mut p = POINT { x: pt.x, y: pt.y };
        unsafe {
            let _ = ScreenToClient(self.hwnd(), &mut p);
        }
        let (x, y) = g.unzoom(p.x as f32, p.y as f32);
        let s = g.scale.get();
        (x / s, y / s)
    }

    /// Effect for the current spot. `effect` in = what the source allows, out = what we do.
    unsafe fn over(
        &self,
        keys: windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS,
        pt: &windows::Win32::Foundation::POINTL,
        effect: *mut windows::Win32::System::Ole::DROPEFFECT,
    ) {
        use windows::Win32::System::Ole::{DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_MOVE};
        let files = self.files.borrow();
        let allowed = !files.is_empty()
            && drop_fn(self.hwnd()).is_some_and(|(g, f)| {
                let (x, y) = self.at(&g, pt);
                f(DropEv::Over(x, y, &files))
            });
        let ctrl = keys.0 & windows::Win32::System::SystemServices::MK_CONTROL.0 != 0;
        let want = drop_effect(!files.is_empty(), ctrl, allowed);
        if effect.is_null() {
            return;
        }
        let ok = unsafe { (*effect).0 };
        // the source may not allow a move (read-only place) or a copy: take what it offers
        let e = if want & ok != 0 {
            want
        } else if want != 0 && ok & DROPEFFECT_COPY.0 != 0 {
            DROPEFFECT_COPY.0
        } else if want != 0 && ok & DROPEFFECT_MOVE.0 != 0 {
            DROPEFFECT_MOVE.0
        } else {
            0
        };
        unsafe { *effect = DROPEFFECT(e) };
    }

    fn leave(&self) {
        self.files.borrow_mut().clear();
        if let Some((_, f)) = drop_fn(self.hwnd()) {
            f(DropEv::Leave);
        }
    }
}

impl windows::Win32::System::Ole::IDropTarget_Impl for DropTarget_Impl {
    fn DragEnter(
        &self,
        data: windows::core::Ref<'_, windows::Win32::System::Com::IDataObject>,
        keys: windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS,
        pt: &windows::Win32::Foundation::POINTL,
        effect: *mut windows::Win32::System::Ole::DROPEFFECT,
    ) -> windows::core::Result<()> {
        *self.files.borrow_mut() = data.ok().map(hdrop_files).unwrap_or_default();
        unsafe {
            self.over(keys, pt, effect);
            if let (Some(h), Ok(d), false) = (&self.helper, data.ok(), effect.is_null()) {
                let _ = h.DragEnter(self.hwnd(), d, &POINT { x: pt.x, y: pt.y }, *effect);
            }
        }
        Ok(())
    }

    fn DragOver(
        &self,
        keys: windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS,
        pt: &windows::Win32::Foundation::POINTL,
        effect: *mut windows::Win32::System::Ole::DROPEFFECT,
    ) -> windows::core::Result<()> {
        unsafe {
            self.over(keys, pt, effect);
            if let (Some(h), false) = (&self.helper, effect.is_null()) {
                let _ = h.DragOver(&POINT { x: pt.x, y: pt.y }, *effect);
            }
        }
        Ok(())
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        if let Some(h) = &self.helper {
            unsafe {
                let _ = h.DragLeave();
            }
        }
        self.leave();
        Ok(())
    }

    fn Drop(
        &self,
        data: windows::core::Ref<'_, windows::Win32::System::Com::IDataObject>,
        keys: windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS,
        pt: &windows::Win32::Foundation::POINTL,
        effect: *mut windows::Win32::System::Ole::DROPEFFECT,
    ) -> windows::core::Result<()> {
        use windows::Win32::System::Ole::{DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE};
        if let Ok(d) = data.ok() {
            *self.files.borrow_mut() = hdrop_files(d);
        }
        unsafe {
            self.over(keys, pt, effect);
            if let (Some(h), Ok(d), false) = (&self.helper, data.ok(), effect.is_null()) {
                let _ = h.Drop(d, &POINT { x: pt.x, y: pt.y }, *effect);
            }
        }
        let e = if effect.is_null() { 0 } else { unsafe { (*effect).0 } };
        let files = std::mem::take(&mut *self.files.borrow_mut());
        let done = e != 0
            && drop_fn(self.hwnd()).is_some_and(|(g, f)| {
                let (x, y) = self.at(&g, pt);
                f(DropEv::Drop(x, y, &files, e == DROPEFFECT_COPY.0))
            });
        if let Some((_, f)) = drop_fn(self.hwnd()) {
            f(DropEv::Leave);
        }
        // a move done by us (optimized move): the source must not delete anything -> NONE
        if !effect.is_null() {
            let out = if done && e == DROPEFFECT_COPY.0 { DROPEFFECT_COPY } else { DROPEFFECT_NONE };
            unsafe { *effect = DROPEFFECT(out.0) };
        }
        Ok(())
    }
}

impl GfWindow {
    fn new() -> Result<Rc<Self>, PlatformError> {
        let hwnd = create_hwnd()?;
        let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
        let scale = dpi as f32 / 96.0;
        let me = Rc::new_cyclic(|weak: &Weak<GfWindow>| GfWindow {
            window: slint::Window::new(weak.clone() as Weak<dyn WindowAdapter>),
            renderer: SoftwareRenderer::new_with_repaint_buffer_type(RepaintBufferType::NewBuffer),
            hwnd: Cell::new(hwnd),
            size: Cell::new(PhysicalSize::new(1, 1)),
            scale: Cell::new(scale),
            dpi: Cell::new(scale),
            zoom: Cell::new(1.0),
            kind: Cell::new(Kind::Panel),
            pinned: Cell::new(false),
            tracking: Cell::new(false),
            dib: RefCell::new(None),
            on_deactivate: RefCell::new(None),
            drag: Cell::new(None),
            on_moved: RefCell::new(None),
            surrogate: Cell::new(0),
            zview: Cell::new(None),
            zoom_snap: RefCell::new(ZoomSnap::default()),
            last_zoom_rect: Cell::new(None),
            zdib: RefCell::new(None),
            glide: RefCell::new(None),
            on_drop: RefCell::new(None),
            drop_reg: Cell::new(false),
        });
        WINDOWS.with(|m| m.borrow_mut().insert(hwnd.0 as isize, Rc::downgrade(&me)));
        ALL.with(|v| v.borrow_mut().push(Rc::downgrade(&me)));
        // now wndproc knows us -> recompute frame (WM_NCCALCSIZE removes it)
        unsafe {
            let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED);
        }
        me.window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: scale });
        Ok(me)
    }

    fn logical(&self, lp: LPARAM) -> LogicalPosition {
        let x = (lp.0 & 0xffff) as i16 as f32;
        let y = ((lp.0 >> 16) & 0xffff) as i16 as f32;
        let s = self.scale.get();
        let (x, y) = self.unzoom(x, y);
        LogicalPosition::new(x / s, y / s)
    }

    /// Client px -> content px (differs only while the open / close animation scales the content).
    fn unzoom(&self, x: f32, y: f32) -> (f32, f32) {
        match self.zview.get() {
            Some((r, _)) => {
                let dw = (r.right - r.left).max(1) as f32;
                let dh = (r.bottom - r.top).max(1) as f32;
                let sw = self.size.get().width.max(1) as f32;
                let sh = self.size.get().height.max(1) as f32;
                let k = (dw / sw).min(dh / sh);
                let cw = (sw * k).round().clamp(1.0, dw);
                let ch = (sh * k).round().clamp(1.0, dh);
                let ox = ((dw - cw) / 2.0).floor();
                let oy = ((dh - ch) / 2.0).floor();
                ((x - ox) / k, (y - oy) / k)
            }
            None => (x, y),
        }
    }

    /// Scale the rendered content into the w x h animation frame (zdib). False = no pixels.
    unsafe fn zoom_into(&self, ref_dc: HDC, w: u32, h: u32) -> bool {
        let Some((_, fade)) = self.zview.get() else { return false };
        let PhysicalSize { width: sw, height: sh } = self.size.get();
        let dib = self.dib.borrow();
        let Some(src) = dib.as_ref() else { return false };
        let mut z = self.zdib.borrow_mut();
        if z.as_ref().map(|d| (d.w, d.h)) != Some((w, h)) {
            *z = make_dib(ref_dc, w, h);
        }
        let Some(d) = z.as_ref() else { return false };
        let s = std::slice::from_raw_parts(src.bits, (sw * sh) as usize);
        let o = std::slice::from_raw_parts_mut(d.bits, (w * h) as usize);
        scale_pixels(s, sw as usize, sh as usize, o, w as usize, h as usize, fade);
        true
    }

    /// Ease the window to rect `to` (screen px), then report the final rect (tile settles onto grid).
    fn glide_to(self: &Rc<Self>, to: RECT) {
        let mut from = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd.get(), &mut from);
        }
        let start = Instant::now();
        let weak = Rc::downgrade(self);
        let t = slint::Timer::default();
        t.start(slint::TimerMode::Repeated, FRAME, move || {
            let Some(g) = weak.upgrade() else { return };
            if g.glide.borrow().is_none() {
                return; // finished
            }
            let k = (start.elapsed().as_secs_f32() * 1000.0 / GLIDE_MS).min(1.0);
            let e = 1.0 - (1.0 - k).powi(3); // ease-out cubic
            let mix = |a: i32, b: i32| a + ((b - a) as f32 * e).round() as i32;
            let (x, y) = (mix(from.left, to.left), mix(from.top, to.top));
            let (w, h) = (mix(from.right - from.left, to.right - to.left), mix(from.bottom - from.top, to.bottom - to.top));
            unsafe {
                let _ = SetWindowPos(g.hwnd.get(), None, x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
            }
            g.resize_physical(w as u32, h as u32);
            if k >= 1.0 {
                // stop: drop the timer later (we are inside its callback)
                if let Some(t) = g.glide.borrow_mut().take() {
                    slint::Timer::single_shot(Duration::ZERO, move || drop(t));
                }
                if let Some(f) = g.on_moved.borrow().clone() {
                    f(to);
                }
            }
        });
        *self.glide.borrow_mut() = Some(t);
    }

    fn mouse_move(&self, p: LogicalPosition) {
        if self.drag_to() {
            return;
        }
        self.window.dispatch_event(WindowEvent::PointerMoved { position: p });
    }

    fn mouse_down(&self, p: LogicalPosition, button: PointerEventButton) {
        if button == PointerEventButton::Left && self.kind.get() == Kind::Tile {
            self.glide.borrow_mut().take();
            let mut d = Drag { resize: self.in_grip(p), cursor: cursor_pos(), rect: RECT::default(), moved: false };
            unsafe {
                let _ = GetWindowRect(self.hwnd.get(), &mut d.rect);
            }
            self.drag.set(Some(d));
        }
        self.window.dispatch_event(WindowEvent::PointerPressed { position: p, button });
    }

    /// `drag` = taken by the caller (wndproc must take it before ReleaseCapture clears it).
    fn mouse_up(self: &Rc<Self>, p: LogicalPosition, button: PointerEventButton, drag: Option<Drag>) {
        if drag.is_some_and(|d| d.moved) {
            // drag ended: release outside -> TouchArea gives no click
            self.window.dispatch_event(WindowEvent::PointerReleased {
                position: LogicalPosition::new(-1.0, -1.0),
                button: PointerEventButton::Left,
            });
            let mut r = RECT::default();
            unsafe {
                let _ = GetWindowRect(self.hwnd.get(), &mut r);
            }
            if self.kind.get() == Kind::Tile {
                // land: size on whole cells, place on grid (setting) -> short glide there
                let s = self.scale.get();
                let (w, h) = snap_size(r.right - r.left, r.bottom - r.top, s);
                let (x, y) = if SNAP.with(|v| v.get()) { snap_pos(r.left, r.top, s) } else { (r.left, r.top) };
                self.glide_to(RECT { left: x, top: y, right: x + w, bottom: y + h });
            } else if let Some(f) = self.on_moved.borrow().clone() {
                f(r);
            }
            return;
        }
        self.window.dispatch_event(WindowEvent::PointerReleased { position: p, button });
    }

    fn in_grip(&self, p: LogicalPosition) -> bool {
        let s = self.scale.get();
        let PhysicalSize { width: w, height: h } = self.size.get();
        p.x > w as f32 / s - GRIP && p.y > h as f32 / s - GRIP
    }

    /// Tile drag step. Returns true while a real drag (not a click) is going on.
    fn drag_to(&self) -> bool {
        let Some(mut d) = self.drag.get() else { return false };
        let c = cursor_pos();
        let (dx, dy) = (c.x - d.cursor.x, c.y - d.cursor.y);
        if !d.moved && dx.abs() <= DRAG_START && dy.abs() <= DRAG_START {
            return false;
        }
        if !d.moved && self.kind.get() == Kind::Tile {
            // dragged tile goes over its neighbours (was: slid under them): the others step to the bottom
            let me = self.hwnd.get();
            ALL.with(|v| {
                for o in v.borrow().iter().filter_map(|w| w.upgrade()) {
                    if o.kind.get() == Kind::Tile && o.hwnd.get() != me {
                        unsafe {
                            let _ = SetWindowPos(o.hwnd.get(), Some(HWND_BOTTOM), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                        }
                    }
                }
            });
        }
        d.moved = true;
        self.drag.set(Some(d));
        if d.resize {
            // follows the cursor smoothly; whole cells on release (glide). Not below 2x2 cells.
            let (mw, mh) = snap_size(0, 0, self.scale.get());
            let w = (d.rect.right - d.rect.left + dx).max(mw);
            let h = (d.rect.bottom - d.rect.top + dy).max(mh);
            self.resize_physical(w as u32, h as u32);
        } else {
            // free move under the cursor; grid (if on) only on release
            let (x, y) = (d.rect.left + dx, d.rect.top + dy);
            unsafe {
                let _ = SetWindowPos(
                    self.hwnd.get(),
                    None,
                    x,
                    y,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }
        true
    }

    fn resize_physical(&self, w: u32, h: u32) {
        if self.size.get() == PhysicalSize::new(w, h) {
            return;
        }
        self.size.set(PhysicalSize::new(w, h));
        if self.zview.get().is_none() {
            unsafe {
                let _ = SetWindowPos(self.hwnd.get(), None, 0, 0, w as i32, h as i32, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
            }
        }
        let s = self.scale.get();
        self.window.dispatch_event(WindowEvent::Resized { size: LogicalSize::new(w as f32 / s, h as f32 / s) });
        self.request_redraw();
    }

    /// Render Slint scene into the DIB (created/resized as needed). Returns false if no DIB.
    unsafe fn draw(&self, ref_dc: HDC) -> bool {
        let PhysicalSize { width: w, height: h } = self.size.get();
        let mut dib = self.dib.borrow_mut();
        if dib.as_ref().map(|d| (d.w, d.h)) != Some((w, h)) {
            *dib = make_dib(ref_dc, w, h);
        }
        let Some(d) = dib.as_ref() else { return false };
        let buf = std::slice::from_raw_parts_mut(d.bits, (w * h) as usize);
        buf.fill(Bgra::default());
        self.renderer.render(buf, w as usize);
        if self.zview.get().is_some() {
            ZVIEW_RENDERS.with(|c| c.set(c.get() + 1));
        }
        REC.with(|r| {
            let mut r = r.borrow_mut();
            let Some(rec) = r.as_mut().filter(|r| r.hwnd == self.hwnd.get().0 as isize && Instant::now() < r.until) else { return };
            // whole content (zoomed open/close frames are scaled copies of it, not recorded)
            let (ox, oy, fw, fh) = (0u32, 0u32, w, h);
            let (fw, fh) = (fw.min(w.saturating_sub(ox)), fh.min(h.saturating_sub(oy)));
            if fw > 0 && fh > 0 {
                rec.frames.push((fw, fh, rgba(buf, w, ox, oy, fw, fh)));
            }
        });
        true
    }

    /// WM_PAINT + BitBlt. Panel: DWM reads alpha via the hidden extended frame.
    /// Tile: content is opaque (own blurred wallpaper), DWM rounds the corners.
    /// Copy the rendered DIB to the window now (shown part only while animating).
    unsafe fn blit(&self) {
        let hwnd = self.hwnd.get();
        let hdc = GetDC(Some(hwnd));
        self.blit_to(hdc);
        let _ = ReleaseDC(Some(hwnd), hdc);
    }

    /// Rendered frame -> window DC: the scaled animation frame while zoomed, else the content.
    unsafe fn blit_to(&self, hdc: HDC) {
        let zoomed = self.zview.get().is_some();
        let src = if zoomed { self.zdib.borrow() } else { self.dib.borrow() };
        if let Some(d) = src.as_ref() {
            let _ = BitBlt(hdc, 0, 0, d.w as i32, d.h as i32, Some(d.dc), 0, 0, SRCCOPY);
        }
    }

    fn paint(&self) {
        let hwnd = self.hwnd.get();
        let PhysicalSize { width: w, height: h } = self.size.get();
        let mut ps = PAINTSTRUCT::default();
        unsafe {
            let hdc = BeginPaint(hwnd, &mut ps);
            if let Some((r, _)) = self.zview.get() {
                let (zw, zh) = ((r.right - r.left).max(1) as u32, (r.bottom - r.top).max(1) as u32);
                let need_draw = self.zoom_snap.borrow_mut().frame()
                    || self.dib.borrow().as_ref().map(|d| (d.w, d.h)) != Some((w, h));
                let drew = if need_draw { self.draw(hdc) } else { true };
                if drew {
                    if need_draw || self.zdib.borrow().as_ref().map(|d| (d.w, d.h)) != Some((zw, zh)) {
                        self.zoom_into(hdc, zw, zh);
                    }
                    self.blit_to(hdc);
                }
            } else {
                let t = Instant::now();
                if self.draw(hdc) {
                    let k = self.kind.get() as u8;
                    perf(|| format!("paint k{k} {w}x{h} {:.2}ms", t.elapsed().as_secs_f64() * 1000.0));
                    self.blit_to(hdc);
                }
            }
            let _ = EndPaint(hwnd, &ps);
        }
    }
}

unsafe fn make_dib(hdc: HDC, w: u32, h: u32) -> Option<Dib> {
    let bi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            biHeight: -(h as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits = std::ptr::null_mut();
    let bmp = CreateDIBSection(Some(hdc), &bi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
    let dc = CreateCompatibleDC(Some(hdc));
    let old = SelectObject(dc, bmp.into());
    Some(Dib { dc, bmp, old, bits: bits as *mut Bgra, w, h })
}

impl WindowAdapter for GfWindow {
    fn window(&self) -> &slint::Window {
        &self.window
    }
    fn size(&self) -> PhysicalSize {
        self.size.get()
    }
    fn renderer(&self) -> &dyn Renderer {
        &self.renderer
    }
    fn set_visible(&self, visible: bool) -> Result<(), PlatformError> {
        let hwnd = self.hwnd.get();
        unsafe {
            if visible {
                if self.kind.get() == Kind::Hint {
                    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                    let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                    SendMessageW(hwnd, WM_NCACTIVATE, Some(WPARAM(1)), Some(LPARAM(0)));
                    return Ok(());
                }
                if offscreen() {
                    // headless test: never take focus from the user; Slint still sees an active window
                    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                    self.window.dispatch_event(WindowEvent::WindowActiveChanged(true));
                    return Ok(());
                }
                let tile = self.kind.get() == Kind::Tile || self.pinned.get();
                if !tile {
                    // look active from the first frame: real activation lands a few frames later, until then
                    // DWM drew the inactive flat grey instead of acrylic (grey flash at the start of open)
                    SendMessageW(hwnd, WM_NCACTIVATE, Some(WPARAM(1)), Some(LPARAM(0)));
                }
                let _ = ShowWindow(hwnd, if tile { SW_SHOWNOACTIVATE } else { SW_SHOW });
                if tile {
                    if SHOW_DESKTOP.with(|v| v.get()) {
                        sync_desktop_layer();
                    } else {
                        let _ = SetWindowPos(hwnd, Some(HWND_BOTTOM), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                    }
                    SendMessageW(hwnd, WM_NCACTIVATE, Some(WPARAM(1)), Some(LPARAM(0))); // never activated -> set NC-active once
                } else {
                    let _ = SetForegroundWindow(hwnd);
                }
            } else {
                let _ = ShowWindow(hwnd, SW_HIDE);
                // free pixel memory while hidden
                self.dib.borrow_mut().take();
                self.zdib.borrow_mut().take();
            }
        }
        Ok(())
    }
    fn position(&self) -> Option<PhysicalPosition> {
        let mut r = Default::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd.get(), &mut r);
        }
        Some(PhysicalPosition::new(r.left, r.top))
    }
    fn set_position(&self, position: slint::WindowPosition) {
        let p = position.to_physical(self.scale.get());
        unsafe {
            let _ = SetWindowPos(self.hwnd.get(), None, p.x, p.y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }
    fn set_size(&self, size: slint::WindowSize) {
        let s = size.to_physical(self.scale.get());
        self.resize_physical(s.width, s.height);
    }
    fn request_redraw(&self) {
        if self.zview.get().is_some() {
            self.zoom_snap.borrow_mut().mark_dirty();
        }
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd.get()), None, false);
        }
    }
    fn update_window_properties(&self, properties: WindowProperties<'_>) {
        if self.kind.get() != Kind::Panel {
            return; // tile / hint size is set from Rust (tile: saved per folder, user can resize)
        }
        // Our components have fixed width/height -> use preferred size.
        let c = properties.layout_constraints();
        let s = self.scale.get();
        let w = (c.preferred.width * s).round().max(1.0) as u32;
        let h = (c.preferred.height * s).round().max(1.0) as u32;
        self.resize_physical(w, h);
    }
}

// ---------- native window + message handling ----------

const WM_MOUSELEAVE: u32 = 0x02A3;
const WM_TILES_DOWN: u32 = WM_APP + 7; // put tiles under pinned panels
const CLASS: PCWSTR = w!("GlassFoldersWnd");

fn create_hwnd() -> Result<HWND, PlatformError> {
    unsafe {
        let inst = GetModuleHandleW(None).map_err(|e| PlatformError::Other(e.to_string()))?;
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: inst.into(),
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassExW(&wc); // fails harmlessly after the first time
        // headless test: layered alpha 0 + click-through = invisible, mouse goes to the user's windows
        let ex = if offscreen() { WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE } else { WS_EX_TOOLWINDOW };
        let hwnd = CreateWindowExW(
            ex,
            CLASS,
            w!("Glass Folders"),
            // Thick frame = DWM treats it as a framed window -> per-pixel alpha + backdrop work.
            // The frame itself is removed in WM_NCCALCSIZE, so client = whole window.
            WS_POPUP | WS_CAPTION | WS_THICKFRAME,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(inst.into()),
            None,
        )
        .map_err(|e| PlatformError::Other(e.to_string()))?;
        if offscreen() {
            let _ = SetLayeredWindowAttributes(hwnd, windows::Win32::Foundation::COLORREF(0), 0, LWA_ALPHA);
        }
        Ok(hwnd)
    }
}

/// Virtual key -> Slint key (keys that have no text of their own, or Slint wants as key codes).
fn key_of(vk: u16) -> Option<Key> {
    Some(match VIRTUAL_KEY(vk) {
        VK_ESCAPE => Key::Escape,
        VK_RETURN => Key::Return,
        VK_BACK => Key::Backspace,
        VK_DELETE => Key::Delete,
        VK_TAB => Key::Tab,
        VK_LEFT => Key::LeftArrow,
        VK_RIGHT => Key::RightArrow,
        VK_UP => Key::UpArrow,
        VK_DOWN => Key::DownArrow,
        VK_HOME => Key::Home,
        VK_END => Key::End,
        VK_SHIFT => Key::Shift,
        VK_CONTROL => Key::Control,
        _ => return None,
    })
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let Some(g) = lookup(hwnd) else { return DefWindowProcW(hwnd, msg, wp, lp) };
    let btn = |m: u32| match m {
        WM_LBUTTONDOWN | WM_LBUTTONUP => PointerEventButton::Left,
        WM_RBUTTONDOWN | WM_RBUTTONUP => PointerEventButton::Right,
        _ => PointerEventButton::Middle,
    };
    match msg {
        WM_PAINT => {
            g.paint();
            return LRESULT(0);
        }
        WM_ERASEBKGND => return LRESULT(1),
        WM_MOUSEMOVE => {
            if !g.tracking.get() {
                let mut t = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                let _ = TrackMouseEvent(&mut t);
                g.tracking.set(true);
            }
            g.mouse_move(g.logical(lp));
            return LRESULT(0);
        }
        WM_MOUSELEAVE => {
            g.tracking.set(false);
            g.window.dispatch_event(WindowEvent::PointerExited);
            return LRESULT(0);
        }
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
            SetCapture(hwnd);
            g.mouse_down(g.logical(lp), btn(msg));
            return LRESULT(0);
        }
        WM_LBUTTONUP | WM_RBUTTONUP | WM_MBUTTONUP => {
            // take drag first: ReleaseCapture sends WM_CAPTURECHANGED, which clears it
            let drag = if msg == WM_LBUTTONUP { g.drag.take() } else { None };
            let _ = ReleaseCapture();
            g.mouse_up(g.logical(lp), btn(msg), drag);
            return LRESULT(0);
        }
        // Keys: special keys from WM_KEYDOWN/UP, typed text from WM_CHAR (TranslateMessage).
        WM_KEYDOWN | WM_KEYUP => {
            let down = msg == WM_KEYDOWN;
            let text: Option<slint::SharedString> = match key_of(wp.0 as u16) {
                Some(k) => Some(k.into()),
                // Ctrl+letter: WM_CHAR gives a control char, Slint shortcuts want the letter
                None if (b'A' as u16..=b'Z' as u16).contains(&(wp.0 as u16)) && GetKeyState(VK_CONTROL.0 as i32) < 0 => {
                    Some(((wp.0 as u8) as char).to_ascii_lowercase().to_string().into())
                }
                None => None,
            };
            if let Some(t) = text {
                g.window.dispatch_event(if down { WindowEvent::KeyPressed { text: t } } else { WindowEvent::KeyReleased { text: t } });
                return LRESULT(0);
            }
        }
        WM_CHAR => {
            let u = wp.0 as u16;
            if (0xD800..0xDC00).contains(&u) {
                g.surrogate.set(u); // high half, wait for low half
                return LRESULT(0);
            }
            let hi = g.surrogate.replace(0);
            let units: Vec<u16> = if (0xDC00..0xE000).contains(&u) && hi != 0 { vec![hi, u] } else { vec![u] };
            let s = String::from_utf16_lossy(&units);
            // control chars (Enter, Backspace, Esc, Ctrl+x) already came as keys
            if !s.chars().any(|c| c.is_control()) {
                let t: slint::SharedString = s.into();
                g.window.dispatch_event(WindowEvent::KeyPressed { text: t.clone() });
                g.window.dispatch_event(WindowEvent::KeyReleased { text: t });
            }
            return LRESULT(0);
        }
        WM_CAPTURECHANGED => g.drag.set(None),
        WM_SETCURSOR if g.kind.get() == Kind::Tile && (lp.0 & 0xffff) as u32 == HTCLIENT => {
            let mut c = POINT::default();
            let _ = GetCursorPos(&mut c);
            let _ = ScreenToClient(hwnd, &mut c);
            let s = g.scale.get();
            if g.in_grip(LogicalPosition::new(c.x as f32 / s, c.y as f32 / s)) {
                SetCursor(LoadCursorW(None, IDC_SIZENWSE).ok());
                return LRESULT(1);
            }
        }
        WM_MOUSEACTIVATE if g.kind.get() != Kind::Panel => return LRESULT(MA_NOACTIVATE as isize),
        // Tile / hint / pinned panel: report NC "always active" -> DWM keeps the system backdrop live (else solid gray).
        // lParam -1: DefWindowProc skips NC repaint. Focus/foreground untouched.
        // always NC-active: live acrylic. Panel too: it closes on focus loss, and its fold-back ran on the
        // inactive flat grey backdrop (grey box at the end of close).
        WM_NCACTIVATE => return DefWindowProcW(hwnd, msg, WPARAM(1), LPARAM(-1)),
        WM_ACTIVATE => {
            let active = (wp.0 & 0xffff) as u32 != WA_INACTIVE;
            g.window.dispatch_event(WindowEvent::WindowActiveChanged(active));
            if !active {
                if let Some(f) = g.on_deactivate.borrow().clone() {
                    slint::Timer::single_shot(Duration::ZERO, move || f());
                }
            }
        }
        WM_SYSCOMMAND if z_layer(g.kind.get(), g.pinned.get()).is_some() && (wp.0 & 0xFFF0) == SC_MINIMIZE as usize => {
            return LRESULT(0);
        }
        WM_WINDOWPOSCHANGING if z_layer(g.kind.get(), g.pinned.get()).is_some() => {
            // keep tiles + pinned panels at the bottom in normal mode, or above desktop host during Win+D
            if !REORDERING.with(|r| r.get()) {
                let pos = &mut *(lp.0 as *mut WINDOWPOS);
                if (pos.flags & SWP_NOZORDER).0 == 0 {
                    let sd = SHOW_DESKTOP.with(|v| v.get());
                    match z_place(g.kind.get(), g.pinned.get(), sd) {
                        Some(ZPlace::Bottom) => {
                            pos.hwndInsertAfter = HWND_BOTTOM;
                            if g.pinned.get() {
                                // pinned panel went to the bottom: tiles go under it again (after this change lands)
                                let _ = PostMessageW(Some(hwnd), WM_TILES_DOWN, WPARAM(0), LPARAM(0));
                            }
                        }
                        Some(ZPlace::AboveHost) => {
                            if pos.hwndInsertAfter == HWND_BOTTOM {
                                pos.flags |= SWP_NOZORDER;
                            }
                        }
                        None => {}
                    }
                }
            }
        }
        WM_TILES_DOWN => {
            if SHOW_DESKTOP.with(|v| v.get()) {
                sync_desktop_layer();
            } else {
                for t in ALL.with(|v| v.borrow().iter().filter_map(|w| w.upgrade()).collect::<Vec<_>>()) {
                    if t.kind.get() == Kind::Tile && IsWindowVisible(t.hwnd.get()).as_bool() {
                        let _ = SetWindowPos(t.hwnd.get(), Some(HWND_BOTTOM), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                    }
                }
            }
            return LRESULT(0);
        }
        WM_DPICHANGED => {
            let dpi = (wp.0 & 0xffff) as f32 / 96.0;
            g.dpi.set(dpi);
            let scale = dpi * g.zoom.get();
            g.scale.set(scale);
            g.window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: scale });
        }
        WM_NCCALCSIZE if wp.0 != 0 => return LRESULT(0),
        // Thick frame -> Windows clamps size to a min track size (~136x39); this lifts the width part
        // (open animation starts at the tile's real size). Height stays >= ~39 on framed windows: Hint has no frame.
        WM_GETMINMAXINFO => {
            let mm = &mut *(lp.0 as *mut MINMAXINFO);
            mm.ptMinTrackSize = POINT { x: 1, y: 1 };
            return LRESULT(0);
        }
        WM_NCHITTEST if g.kind.get() == Kind::Hint => return LRESULT(-1), // HTTRANSPARENT: mouse goes to the window below
        WM_NCHITTEST => return LRESULT(1), // HTCLIENT: no resize borders
        // Alt+F4 on a folder window: ignored (Esc / X close it). Close request to any other window of ours =
        // taskkill without /F, Task Manager "End task": quit cleanly (folders back).
        WM_CLOSE => {
            if g.kind.get() != Kind::Panel {
                quit_now();
            }
            return LRESULT(0);
        }
        WM_QUERYENDSESSION => return LRESULT(1),
        // log off / shut down: the process may be killed right after this returns -> restore now
        WM_ENDSESSION if wp.0 != 0 => {
            quit_now();
            return LRESULT(0);
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

// ---------- platform + event loop ----------

struct Proxy {
    thread: u32,
    queue: Arc<Mutex<Vec<Box<dyn FnOnce() + Send>>>>,
    quit: Arc<AtomicBool>,
}

impl EventLoopProxy for Proxy {
    fn quit_event_loop(&self) -> Result<(), EventLoopError> {
        self.quit.store(true, Ordering::SeqCst);
        unsafe {
            let _ = PostThreadMessageW(self.thread, WM_NULL, WPARAM(0), LPARAM(0));
        }
        Ok(())
    }
    fn invoke_from_event_loop(&self, f: Box<dyn FnOnce() + Send>) -> Result<(), EventLoopError> {
        self.queue.lock().unwrap().push(f);
        unsafe {
            let _ = PostThreadMessageW(self.thread, WM_NULL, WPARAM(0), LPARAM(0));
        }
        Ok(())
    }
}

pub struct GfPlatform {
    start: Instant,
    thread: u32,
    queue: Arc<Mutex<Vec<Box<dyn FnOnce() + Send>>>>,
    quit: Arc<AtomicBool>,
}

impl GfPlatform {
    pub fn new() -> Self {
        unsafe {
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }
        GfPlatform {
            start: Instant::now(),
            thread: unsafe { GetCurrentThreadId() },
            queue: Default::default(),
            quit: Default::default(),
        }
    }
}

impl Platform for GfPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(GfWindow::new()?)
    }

    fn duration_since_start(&self) -> Duration {
        self.start.elapsed()
    }

    fn new_event_loop_proxy(&self) -> Option<Box<dyn EventLoopProxy>> {
        Some(Box::new(Proxy { thread: self.thread, queue: self.queue.clone(), quit: self.quit.clone() }))
    }

    fn run_event_loop(&self) -> Result<(), PlatformError> {
        struct HookGuard(HWINEVENTHOOK);
        impl Drop for HookGuard {
            fn drop(&mut self) {
                if !self.0.is_invalid() {
                    unsafe {
                        let _ = UnhookWinEvent(self.0);
                    }
                }
            }
        }
        let _fg_hook = if offscreen() {
            HookGuard(HWINEVENTHOOK(std::ptr::null_mut()))
        } else {
            unsafe {
                HookGuard(SetWinEventHook(
                    EVENT_SYSTEM_FOREGROUND,
                    EVENT_SYSTEM_FOREGROUND,
                    None,
                    Some(fg_hook),
                    0,
                    0,
                    WINEVENT_OUTOFCONTEXT,
                ))
            }
        };

        let mut msg = MSG::default();
        loop {
            slint::platform::update_timers_and_animations();
            let jobs: Vec<_> = std::mem::take(&mut *self.queue.lock().unwrap());
            for f in jobs {
                f();
            }
            if self.quit.load(Ordering::SeqCst) {
                return Ok(());
            }
            // Animation running (Slint's own, or a frame timer of ours): one pass per screen refresh,
            // paced by DwmFlush (waits for the next composition). Was: a 16 ms wait / 8 ms timers on the
            // ~15.6 ms system tick -> 50-60 fps on a 120 Hz screen with uneven steps (judder).
            // Else sleep until the next timer or input (idle = 0 CPU).
            let mut animating = false;
            ALL.with(|v| {
                for g in v.borrow().iter().filter_map(|w| w.upgrade()) {
                    if g.window.has_active_animations() {
                        animating = true;
                        g.request_redraw();
                    }
                }
            });
            let next = slint::platform::duration_until_next_timer_update();
            let paced = animating || next.is_some_and(|d| d <= Duration::from_millis(12));
            let timeout = if paced {
                0
            } else {
                next.map(|d| d.as_millis().min(u32::MAX as u128 - 1) as u32).unwrap_or(u32::MAX) // INFINITE
            };
            unsafe {
                if timeout > 0 {
                    MsgWaitForMultipleObjects(None, false, timeout, QS_ALLINPUT);
                }
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    if msg.message == WM_QUIT {
                        return Ok(());
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                if paced {
                    let _ = DwmFlush();
                }
            }
        }
    }
}
// ---------- headless test hooks (GF_SCRIPT) ----------
// Script events go straight to Slint (dispatch_event); the real cursor is never read or moved.
// Drag code reads cursor_pos(): in script mode = virtual cursor set by inject().

thread_local! {
    static OFFSCREEN: Cell<bool> = const { Cell::new(false) };
    static VCURSOR: Cell<Option<POINT>> = const { Cell::new(None) };
    static REC: RefCell<Option<Rec>> = const { RefCell::new(None) };
}

struct Rec {
    hwnd: isize,
    until: Instant,
    frames: Vec<(u32, u32, Vec<u8>)>,
}

thread_local! {
    /// Script mode: answer for the next native popup menu (no modal menu loop in tests).
    static MENU_PICK: Cell<Option<u32>> = const { Cell::new(None) };
}

/// Script: the next shortcut menu picks item `id` without showing.
pub fn set_menu_pick(id: u32) {
    MENU_PICK.with(|v| v.set(Some(id)));
}

/// Scripted pick (taken once); in script mode with none queued = dismissed. None = show the real menu.
pub fn take_menu_pick() -> Option<u32> {
    let p = MENU_PICK.with(|v| v.take());
    if p.is_some() || VCURSOR.with(|v| v.get()).is_some() { Some(p.unwrap_or(0)) } else { None }
}

/// Windows invisible + click-through + never activated. Call before the first window is made.
pub fn set_offscreen(on: bool) {
    OFFSCREEN.with(|v| v.set(on));
}

pub fn offscreen() -> bool {
    OFFSCREEN.with(|v| v.get())
}

/// Script mode: cursor_pos() = virtual cursor (starts far away), real cursor never read.
pub fn set_virtual_cursor() {
    VCURSOR.with(|v| v.set(Some(POINT { x: -30000, y: -30000 })));
}

/// Cursor on screen (physical px): real, or virtual in script mode.
pub fn cursor_pos() -> POINT {
    VCURSOR.with(|v| v.get()).unwrap_or_else(|| {
        let mut c = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut c);
        }
        c
    })
}

#[derive(Clone, Copy)]
pub enum Ptr {
    Move,
    Down(PointerEventButton),
    Up(PointerEventButton),
    Leave,
}

/// Scripted pointer event at content coords (logical px) of a window. Same path as the real mouse,
/// minus SetCapture / TrackMouseEvent. Returns false if the window is unknown.
pub fn inject(window: &slint::Window, ev: Ptr, x: f32, y: f32) -> bool {
    let Some(g) = find(window) else { return false };
    let s = g.scale.get();
    let mut pt = POINT { x: (x * s).round() as i32, y: (y * s).round() as i32 };
    unsafe {
        let _ = ClientToScreen(g.hwnd.get(), &mut pt);
    }
    VCURSOR.with(|v| v.set(Some(pt)));
    let p = LogicalPosition::new(x, y);
    match ev {
        Ptr::Move => g.mouse_move(p),
        Ptr::Down(b) => g.mouse_down(p, b),
        Ptr::Up(b) => {
            let d = if b == PointerEventButton::Left { g.drag.take() } else { None };
            g.mouse_up(p, b, d)
        }
        Ptr::Leave => g.window.dispatch_event(WindowEvent::PointerExited),
    }
    true
}

pub fn is_shown(window: &slint::Window) -> bool {
    find(window).is_some_and(|g| unsafe { IsWindowVisible(g.hwnd.get()).as_bool() })
}

/// Render the window now and save its whole content as PNG (straight RGBA). Returns (w, h).
pub fn shot(window: &slint::Window, path: &Path) -> Result<(u32, u32), String> {
    let g = find(window).ok_or("unknown window")?;
    if !unsafe { IsWindowVisible(g.hwnd.get()).as_bool() } {
        return Err("window hidden".into());
    }
    let ok = unsafe {
        let dc = GetDC(None);
        let ok = g.draw(dc);
        ReleaseDC(None, dc);
        ok
    };
    let dib = g.dib.borrow();
    let d = dib.as_ref().filter(|_| ok).ok_or("no pixels")?;
    let buf = unsafe { std::slice::from_raw_parts(d.bits, (d.w * d.h) as usize) };
    save_png(path, d.w, d.h, &rgba(buf, d.w, 0, 0, d.w, d.h))?;
    Ok((d.w, d.h))
}

/// Start keeping every frame this window renders for `ms` (animation check).
pub fn record(window: &slint::Window, ms: u64) -> bool {
    let Some(g) = find(window) else { return false };
    REC.with(|r| {
        *r.borrow_mut() = Some(Rec { hwnd: g.hwnd.get().0 as isize, until: Instant::now() + Duration::from_millis(ms), frames: Vec::new() })
    });
    true
}

/// Frames kept by record(): (w, h, straight RGBA).
pub fn take_frames() -> Vec<(u32, u32, Vec<u8>)> {
    REC.with(|r| r.borrow_mut().take()).map(|r| r.frames).unwrap_or_default()
}

/// Premultiplied BGRA sub-rect -> straight RGBA bytes.
fn rgba(buf: &[Bgra], stride: u32, x: u32, y: u32, w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for row in y..y + h {
        let from = (row * stride + x) as usize;
        for p in &buf[from..from + w as usize] {
            let un = |c: u8| if p.a == 0 { 0 } else { ((c as u32 * 255 + p.a as u32 / 2) / p.a as u32).min(255) as u8 };
            out.extend_from_slice(&[un(p.r), un(p.g), un(p.b), p.a]);
        }
    }
    out
}

pub fn save_png(path: &Path, w: u32, h: u32, rgba: &[u8]) -> Result<(), String> {
    let f = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(f), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut wr = enc.write_header().map_err(|e| e.to_string())?;
    wr.write_image_data(rgba).map_err(|e| e.to_string())
}
