// Кратко: свой «мост» между Slint и окнами Windows. Slint рисует картинку в память,
// мы сами показываем её в окне с прозрачностью (чтобы было видно стекло) и передаём мышь/клавиши.
//
// Why: Slint winit+software path presents via softbuffer -> no per-pixel alpha -> glass hidden.
// Here: SoftwareRenderer -> premultiplied BGRA DIB section -> BitBlt into a window whose DWM
// frame is extended into the whole client area, so alpha is honored and the backdrop shows.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
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
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::HiDpi::{GetDpiForWindow, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VIRTUAL_KEY, VK_BACK, VK_CONTROL,
    VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::*;

// ---------- pixel type = DIB layout (premultiplied BGRA) ----------

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Bgra {
    b: u8,
    g: u8,
    r: u8,
    a: u8,
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
    scale: Cell<f32>,
    kind: Cell<Kind>,
    tracking: Cell<bool>,
    dib: RefCell<Option<Dib>>,
    on_deactivate: RefCell<Option<Rc<dyn Fn()>>>,
    drag: Cell<Option<Drag>>,
    on_moved: RefCell<Option<Rc<dyn Fn(RECT)>>>,
    surrogate: Cell<u16>, // WM_CHAR high surrogate waiting for its pair
    /// Animation: window shows only part of its content. (home = where content's top-left is on screen,
    /// shown rect). Slint keeps its full size; paint + mouse are offset.
    view: Cell<Option<(POINT, RECT)>>,
    glide: RefCell<Option<slint::Timer>>,
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
const GLIDE_MS: f32 = 130.0; // tile settles onto the grid after release

thread_local! {
    /// Tiles land on the grid after a move (setting). Size always snaps to whole cells.
    static SNAP: Cell<bool> = const { Cell::new(true) };
    static TILE_NAMED: Cell<bool> = const { Cell::new(true) };
    static WINDOWS: RefCell<HashMap<isize, Weak<GfWindow>>> = RefCell::new(HashMap::new());
    static ALL: RefCell<Vec<Weak<GfWindow>>> = const { RefCell::new(Vec::new()) };
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
        unsafe {
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            let n = if kind == Kind::Tile { ex | WS_EX_NOACTIVATE.0 as isize } else { ex & !(WS_EX_NOACTIVATE.0 as isize) };
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, n);
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
    let mut d = Drag { resize: false, cursor: POINT::default(), rect: RECT::default(), moved: false };
    unsafe {
        let _ = GetCursorPos(&mut d.cursor);
        let _ = GetWindowRect(g.hwnd.get(), &mut d.rect);
    }
    g.drag.set(Some(d));
}

pub fn set_snap(on: bool) {
    SNAP.with(|s| s.set(on));
}

/// Animation frame: window shows `shown` (screen rect) of its content, whose top-left sits at `home`.
/// None = normal (window = content at home). Content size is untouched (no relayout per frame).
pub fn set_view(window: &slint::Window, v: Option<(POINT, RECT)>) {
    let Some(g) = find(window) else { return };
    let was = g.view.replace(v);
    let hwnd = g.hwnd.get();
    let (x, y, w, h) = match (v, was) {
        (Some((_, r)), _) => (r.left, r.top, (r.right - r.left).max(1), (r.bottom - r.top).max(1)),
        (None, Some((home, _))) => {
            let PhysicalSize { width, height } = g.size.get();
            (home.x, home.y, width as i32, height as i32)
        }
        (None, None) => return,
    };
    unsafe {
        let _ = SetWindowPos(hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
        let _ = InvalidateRect(Some(hwnd), None, false);
        let _ = UpdateWindow(hwnd); // paint now: no frame with stale content
    }
}

/// Where the content's top-left is while an animation frame is shown.
pub fn view_home(window: &slint::Window) -> Option<POINT> {
    find(window)?.view.get().map(|(h, _)| h)
}

/// Screen rect of a window (physical px).
pub fn rect_of(window: &slint::Window) -> Option<RECT> {
    let g = find(window)?;
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(g.hwnd.get(), &mut r);
    }
    Some(r)
}

pub fn set_on_deactivate(window: &slint::Window, f: impl Fn() + 'static) {
    if let Some(g) = find(window) {
        *g.on_deactivate.borrow_mut() = Some(Rc::new(f));
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
            kind: Cell::new(Kind::Panel),
            tracking: Cell::new(false),
            dib: RefCell::new(None),
            on_deactivate: RefCell::new(None),
            drag: Cell::new(None),
            on_moved: RefCell::new(None),
            surrogate: Cell::new(0),
            view: Cell::new(None),
            glide: RefCell::new(None),
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
        let (ox, oy) = self.offset();
        LogicalPosition::new((x + ox as f32) / s, (y + oy as f32) / s)
    }

    /// Content px hidden left/above the shown part (animation), else 0.
    fn offset(&self) -> (i32, i32) {
        match self.view.get() {
            Some((home, r)) => (r.left - home.x, r.top - home.y),
            None => (0, 0),
        }
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
        t.start(slint::TimerMode::Repeated, Duration::from_millis(8), move || {
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

    fn in_grip(&self, p: LogicalPosition) -> bool {
        let s = self.scale.get();
        let PhysicalSize { width: w, height: h } = self.size.get();
        p.x > w as f32 / s - GRIP && p.y > h as f32 / s - GRIP
    }

    /// Tile drag step. Returns true while a real drag (not a click) is going on.
    fn drag_to(&self) -> bool {
        let Some(mut d) = self.drag.get() else { return false };
        let mut c = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut c);
        }
        let (dx, dy) = (c.x - d.cursor.x, c.y - d.cursor.y);
        if !d.moved && dx.abs() <= DRAG_START && dy.abs() <= DRAG_START {
            return false;
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
        if self.view.get().is_none() {
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
        true
    }

    /// WM_PAINT + BitBlt. Panel: DWM reads alpha via the hidden extended frame.
    /// Tile: content is opaque (own blurred wallpaper), DWM rounds the corners.
    fn paint(&self) {
        let hwnd = self.hwnd.get();
        let PhysicalSize { width: w, height: h } = self.size.get();
        let mut ps = PAINTSTRUCT::default();
        unsafe {
            let hdc = BeginPaint(hwnd, &mut ps);
            if self.draw(hdc) {
                if let Some(d) = self.dib.borrow().as_ref() {
                    let (ox, oy) = self.offset();
                    let _ = BitBlt(hdc, 0, 0, w as i32, h as i32, Some(d.dc), ox, oy, SRCCOPY);
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
                let tile = self.kind.get() == Kind::Tile;
                let _ = ShowWindow(hwnd, if tile { SW_SHOWNOACTIVATE } else { SW_SHOW });
                if tile {
                    let _ = SetWindowPos(hwnd, Some(HWND_BOTTOM), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                    SendMessageW(hwnd, WM_NCACTIVATE, Some(WPARAM(1)), Some(LPARAM(0))); // never activated -> set NC-active once
                } else {
                    let _ = SetForegroundWindow(hwnd);
                }
            } else {
                let _ = ShowWindow(hwnd, SW_HIDE);
                // free pixel memory while hidden
                self.dib.borrow_mut().take();
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
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd.get()), None, false);
        }
    }
    fn update_window_properties(&self, properties: WindowProperties<'_>) {
        if self.kind.get() == Kind::Tile {
            return; // tile size is set from Rust (saved per folder, user can resize)
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
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
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
        .map_err(|e| PlatformError::Other(e.to_string()))
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
            if g.drag_to() {
                return LRESULT(0);
            }
            g.window.dispatch_event(WindowEvent::PointerMoved { position: g.logical(lp) });
            return LRESULT(0);
        }
        WM_MOUSELEAVE => {
            g.tracking.set(false);
            g.window.dispatch_event(WindowEvent::PointerExited);
            return LRESULT(0);
        }
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
            SetCapture(hwnd);
            if msg == WM_LBUTTONDOWN && g.kind.get() == Kind::Tile {
                g.glide.borrow_mut().take();
                let mut d = Drag { resize: g.in_grip(g.logical(lp)), cursor: POINT::default(), rect: RECT::default(), moved: false };
                let _ = GetCursorPos(&mut d.cursor);
                let _ = GetWindowRect(hwnd, &mut d.rect);
                g.drag.set(Some(d));
            }
            g.window.dispatch_event(WindowEvent::PointerPressed { position: g.logical(lp), button: btn(msg) });
            return LRESULT(0);
        }
        WM_LBUTTONUP | WM_RBUTTONUP | WM_MBUTTONUP => {
            // take drag first: ReleaseCapture sends WM_CAPTURECHANGED, which clears it
            let drag = if msg == WM_LBUTTONUP { g.drag.take() } else { None };
            let _ = ReleaseCapture();
            {
                if let Some(d) = drag {
                    if d.moved {
                        // drag ended: release outside -> TouchArea gives no click
                        g.window.dispatch_event(WindowEvent::PointerReleased {
                            position: LogicalPosition::new(-1.0, -1.0),
                            button: PointerEventButton::Left,
                        });
                        let mut r = RECT::default();
                        let _ = GetWindowRect(hwnd, &mut r);
                        if g.kind.get() == Kind::Tile {
                            // land: size on whole cells, place on grid (setting) -> short glide there
                            let s = g.scale.get();
                            let (w, h) = snap_size(r.right - r.left, r.bottom - r.top, s);
                            let (x, y) = if SNAP.with(|v| v.get()) { snap_pos(r.left, r.top, s) } else { (r.left, r.top) };
                            g.glide_to(RECT { left: x, top: y, right: x + w, bottom: y + h });
                        } else if let Some(f) = g.on_moved.borrow().clone() {
                            f(r);
                        }
                        return LRESULT(0);
                    }
                }
            }
            g.window.dispatch_event(WindowEvent::PointerReleased { position: g.logical(lp), button: btn(msg) });
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
        WM_MOUSEACTIVATE if g.kind.get() == Kind::Tile => return LRESULT(MA_NOACTIVATE as isize),
        // Tile: report NC "always active" -> DWM keeps the system backdrop live (else solid gray).
        // lParam -1: DefWindowProc skips NC repaint. Focus/foreground untouched.
        WM_NCACTIVATE if g.kind.get() == Kind::Tile => return DefWindowProcW(hwnd, msg, WPARAM(1), LPARAM(-1)),
        WM_ACTIVATE => {
            let active = (wp.0 & 0xffff) as u32 != WA_INACTIVE;
            g.window.dispatch_event(WindowEvent::WindowActiveChanged(active));
            if !active {
                if let Some(f) = g.on_deactivate.borrow().clone() {
                    slint::Timer::single_shot(Duration::ZERO, move || f());
                }
            }
        }
        WM_WINDOWPOSCHANGING if g.kind.get() == Kind::Tile => {
            // keep tiles at the bottom whenever z-order changes; plain moves (drag) stay cheap
            let pos = &mut *(lp.0 as *mut WINDOWPOS);
            if (pos.flags & SWP_NOZORDER).0 == 0 {
                pos.hwndInsertAfter = HWND_BOTTOM;
            }
        }
        WM_DPICHANGED => {
            let scale = (wp.0 & 0xffff) as f32 / 96.0;
            g.scale.set(scale);
            g.window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: scale });
        }
        WM_NCCALCSIZE if wp.0 != 0 => return LRESULT(0),
        WM_NCHITTEST => return LRESULT(1), // HTCLIENT: no resize borders
        WM_CLOSE => return LRESULT(0),
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
            // Animations running -> ~60 fps; else sleep until next timer or input (idle = 0 CPU).
            let animating = ALL.with(|v| v.borrow().iter().filter_map(|w| w.upgrade()).any(|g| g.window.has_active_animations()));
            if animating {
                ALL.with(|v| v.borrow().iter().filter_map(|w| w.upgrade()).for_each(|g| g.request_redraw()));
            }
            let timeout = if animating {
                16
            } else {
                slint::platform::duration_until_next_timer_update()
                    .map(|d| d.as_millis().min(u32::MAX as u128 - 1) as u32)
                    .unwrap_or(u32::MAX) // INFINITE
            };
            unsafe {
                MsgWaitForMultipleObjects(None, false, timeout, QS_ALLINPUT);
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    if msg.message == WM_QUIT {
                        return Ok(());
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
    }
}