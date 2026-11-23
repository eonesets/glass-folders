// Кратко: точка входа. При запуске ставит вместо папок на столе плитки (папки скрываются, ярлыки
// и выделение на столе остаются); при выходе значки папок возвращаются на места. Окно папки вырастает из плитки.
// Закреплённая папка висит открытой на столе (своё окно), её плитка спрятана. Подсказка с именем ярлыка при наведении.

#![windows_subsystem = "windows"]

slint::include_modules!();
mod desk;
mod icons;
mod model;
mod plat;
mod shell;
mod win;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use model::{GRect, GAP, HEAD, PITCH, ROW};
use slint::{ComponentHandle, Image, Model, ModelRc, PhysicalPosition, PhysicalSize, VecModel};
use windows::Win32::Foundation::{POINT, RECT};

const OPEN_MS: f32 = 260.0; // panel zooms out of the tile
const CLOSE_MS: f32 = 190.0; // ... and back into it
const HINT_MS: u64 = 400; // pointer rests this long on a shortcut -> its name shows

// Icons are requested at exact on-screen pixel size: big sizes make the shell add a frame around low-res icons.
fn px(logical: f32, scale: f32) -> i32 {
    (logical * scale).round() as i32
}

/// What a dragged shortcut will do on release.
#[derive(Clone, Copy, PartialEq, Default)]
enum Target {
    #[default]
    None,
    /// into group (index), at place
    Group(usize, usize),
    /// new group at App::ghost
    New,
}

/// State of one folder window (panel).
#[derive(Default)]
struct App {
    folder: Option<model::Folder>,
    ids: Vec<PathBuf>,    // slot id -> file
    id_group: Vec<usize>, // slot id -> group index
    // panel layout: base = as rendered, prev = shown now (while dragging), ghost = new-group plate
    base: Vec<GRect>,
    prev: Vec<GRect>,
    ghost: Option<GRect>,
    target: Target,
    grab: Option<(f32, f32)>, // group move: cursor offset from group corner
    lift: [f32; 4],           // moved / resized group as drawn under the cursor (grid px)
    groups: Option<Rc<VecModel<GroupData>>>,
    // open / close animation of the panel: openness 0 (= tile) .. 1 (= panel), and its two ends
    anim: Option<slint::Timer>,
    closing: bool,
    open_p: f32,
    geo: Option<(RECT, RECT)>, // (tile rect, panel rect), screen px
    // group unfolded by its "+N" slot (until folded back or panel closed)
    unfolded: Option<usize>,
    /// pinned open on the desktop: stays on focus loss / launch, its tile is hidden
    pinned: bool,
}

/// App-wide state, shared by all folder windows.
#[derive(Default)]
struct Global {
    set: RefCell<model::Settings>,
    dark: Cell<bool>,
    tiles: RefCell<Vec<(PathBuf, Tile)>>,
    tile_items: RefCell<HashMap<PathBuf, Vec<PathBuf>>>, // folder -> files behind its tile icons (index = icon)
    /// tiles whose icons are still loading: folder -> candidate files (most launched first)
    tile_wait: RefCell<HashMap<PathBuf, Vec<PathBuf>>>,
    /// Folder windows. [0] = the one tiles open; others = pinned (or just unpinned, closing).
    panels: RefCell<Vec<(Panel, Shared)>>,
    hint: RefCell<Option<Hint>>,
    hint_timer: RefCell<Option<slint::Timer>>,
    hint_on: Cell<bool>,
}

thread_local! {
    static GL: &'static Global = Box::leak(Box::default());
}

fn g() -> &'static Global {
    GL.with(|g| *g)
}

impl App {
    /// Group header height in panel px. No title and no dot: plate hugs its slots.
    fn head(&self) -> f32 {
        let s = g().set.borrow();
        if s.names || s.dots {
            HEAD
        } else {
            0.0
        }
    }
    fn tile_rect(&self, dir: &Path) -> Option<RECT> {
        g().tiles.borrow().iter().find(|(d, _)| d == dir).and_then(|(_, t)| plat::rect_of(t.window()))
    }
}

type Shared = Rc<RefCell<App>>;

/// The folder window tiles open.
fn main_panel() -> Option<(Panel, Shared)> {
    g().panels.borrow().first().map(|(p, a)| (p.clone_strong(), a.clone()))
}

fn all_panels() -> Vec<(Panel, Shared)> {
    g().panels.borrow().iter().map(|(p, a)| (p.clone_strong(), a.clone())).collect()
}

fn main() -> Result<(), slint::PlatformError> {
    slint::platform::set_platform(Box::new(plat::GfPlatform::new())).expect("platform");
    win::com_init();
    // any crash: give the user their desktop icons back
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |i| {
        desk::restore_folders(&HashMap::new());
        hook(i);
    }));
    let desktop = win::desktop_dir();
    *g().set.borrow_mut() = model::load_settings();
    g().dark.set(win::system_dark());
    {
        let s = g().set.borrow();
        plat::set_snap(s.snap);
        plat::set_tile_named(s.tnames);
        model::set_header(s.names || s.dots);
    }
    let hint = Hint::new()?;
    plat::set_kind(hint.window(), plat::Kind::Hint);
    if let Some(h) = plat::hwnd_of(hint.window()) {
        win::clear(h);
    }
    hint.set_dark(g().dark.get());
    *g().hint.borrow_mut() = Some(hint);

    let first = make_panel()?;
    g().panels.borrow_mut().push(first);

    // v0.4 left-over (whole icon layer hidden by a killed run) + folder icons parked by a killed run
    desk::unhide_icon_layer();
    let killed: HashMap<PathBuf, POINT> =
        g().set.borrow().homes.iter().map(|(k, [x, y])| (PathBuf::from(k), POINT { x: *x, y: *y })).collect();
    desk::restore_folders(&killed);

    icons::on_ready(icons_ready);
    let homes = make_tiles(&desktop, &killed)?;
    prefetch_icons();
    // tiles stand where the folders were -> hide the folder icons (dev: GF_KEEP_ICONS=1 keeps them).
    // Spots saved first: a killed run gets them back on next start.
    if std::env::var("GF_KEEP_ICONS").is_err() && !homes.is_empty() {
        {
            let mut s = g().set.borrow_mut();
            s.homes = homes.iter().map(|(k, p)| (k.to_string_lossy().into_owned(), [p.x, p.y])).collect();
            model::save_settings(&s);
        }
        desk::hide_folders(homes);
    }

    // pinned folders: open on the desktop again, where they were left
    let pins: Vec<(PathBuf, [i32; 2])> =
        g().tiles.borrow().iter().filter_map(|(d, _)| model::load_cfg(d).pin.map(|p| (d.clone(), p))).collect();
    for (dir, [x, y]) in pins {
        let (p, a) = make_panel()?;
        pin_open(&p, &a, &dir, x, y);
        g().panels.borrow_mut().push((p, a));
    }

    // Dev helper: glass-folders.exe --open GAMES
    let args: Vec<String> = std::env::args().collect();
    if let Some(name) = args.iter().position(|a| a == "--open").and_then(|i| args.get(i + 1)) {
        let d = desktop.join(name);
        slint::Timer::single_shot(Duration::from_millis(150), move || {
            if let Some((p, a)) = main_panel() {
                open_folder(&p, &a, &d);
            }
        });
    }

    let r = slint::run_event_loop();
    give_back_icons();
    r
}

/// Background: icons of every folder at panel size, so the first open is instant too.
fn prefetch_icons() {
    let Some((p, _)) = main_panel() else { return };
    let ipx = px(48.0, icon_scale(p.window()));
    for (d, _) in g().tiles.borrow().iter() {
        let f = model::scan(d);
        icons::want(f.groups.iter().flat_map(|g| g.items.iter().cloned()), ipx, false);
    }
}

/// New folder window (hidden) with all its callbacks.
fn make_panel() -> Result<(Panel, Shared), slint::PlatformError> {
    let app: Shared = Rc::new(RefCell::new(App::default()));
    let dark = g().dark.get();
    let panel = Panel::new()?;
    if let Some(h) = plat::hwnd_of(panel.window()) {
        win::glass(h, dark);
    }
    plat::set_zoom(panel.window(), g().set.borrow().zoom());
    {
        let (a, p) = (app.clone(), panel.as_weak());
        let stay = std::env::var("GF_STAY").is_ok(); // dev: keep panel open on focus loss
        plat::set_on_deactivate(panel.window(), move || {
            if stay || a.borrow().pinned {
                return;
            }
            if let Some(p) = p.upgrade() {
                close(&p, &a);
            }
        });
    }
    {
        // pinned window moved by its header: remember the spot
        let a = app.clone();
        plat::set_on_moved(panel.window(), move |r| {
            let st = a.borrow();
            let (true, Some(f)) = (st.pinned, st.folder.as_ref()) else { return };
            let mut cfg = model::load_cfg(&f.dir);
            cfg.pin = Some([r.left, r.top]);
            let _ = model::save_cfg(&f.dir, &cfg);
        });
    }
    panel.set_dark(dark);
    push_settings(&panel, &g().set.borrow());
    let pal: Vec<slint::Color> = model::PAL.iter().map(|h| model::color(h)).collect();
    panel.set_pal(ModelRc::new(VecModel::from(pal)));

    {
        let (a, p) = (app.clone(), panel.as_weak());
        panel.on_launch(move |id| {
            hide_hint();
            let path = a.borrow().ids.get(id as usize).cloned();
            let root = a.borrow().folder.as_ref().map(|f| f.dir.clone());
            if let Some(path) = path {
                win::open(&path);
                if let Some(root) = root {
                    model::bump_use(&root, &path);
                    refresh_tile(&root);
                }
            }
            if a.borrow().pinned {
                return;
            }
            if let Some(p) = p.upgrade() {
                close(&p, &a);
            }
        });
    }
    {
        let (a, p) = (app.clone(), panel.as_weak());
        panel.on_open_group(move |idx| {
            // deferred: the clicked slot belongs to the model that render() updates
            let (a, p) = (a.clone(), p.clone());
            slint::Timer::single_shot(Duration::ZERO, move || {
                let Some(p) = p.upgrade() else { return };
                {
                    let mut st = a.borrow_mut();
                    let i = idx as usize;
                    st.unfolded = if st.unfolded == Some(i) { None } else { Some(i) };
                }
                render(&p, &a);
                fit_panel(&p);
            });
        });
    }
    {
        let (a, p) = (app.clone(), panel.as_weak());
        panel.on_pick_color(move |idx, c| {
            if let Some(p) = p.upgrade() {
                set_color(&p, &a, idx, Some(model::hex(c)));
            }
        });
    }
    {
        let (a, p) = (app.clone(), panel.as_weak());
        panel.on_clear_color(move |idx| {
            if let Some(p) = p.upgrade() {
                set_color(&p, &a, idx, None);
            }
        });
    }
    {
        let p = panel.as_weak();
        panel.on_drag_start(move || {
            if let Some(p) = p.upgrade() {
                plat::start_move(p.window());
            }
        });
    }
    panel.on_set_opt(move |key, v| set_opt(key.as_str(), v));
    {
        let (a, p) = (app.clone(), panel.as_weak());
        panel.on_close_panel(move || {
            if let Some(p) = p.upgrade() {
                if a.borrow().pinned {
                    set_pin(&p, &a, false); // X on a pinned folder: unpin + fold back into its tile
                }
                close(&p, &a);
            }
        });
    }
    {
        let (a, p) = (app.clone(), panel.as_weak());
        panel.on_toggle_pin(move || {
            if let Some(p) = p.upgrade() {
                let on = !a.borrow().pinned;
                set_pin(&p, &a, on);
            }
        });
    }
    {
        let (a, p) = (app.clone(), panel.as_weak());
        panel.on_hover(move |id, x, y, w, h| {
            // names under icons shown already: no hint
            let name = if id < 0 || g().set.borrow().labels { None } else { a.borrow().ids.get(id as usize).map(|f| model::name_of(f)) };
            if let Some(p) = p.upgrade() {
                hint_at(p.window(), name, x, y, w, h);
            }
        });
    }
    {
        let (a, p) = (app.clone(), panel.as_weak());
        panel.on_item_drag(move |id, x, y, inside| {
            if let Some(p) = p.upgrade() {
                item_drag(&mut a.borrow_mut(), id, x, y, inside);
                show_preview(&p, &a.borrow(), true);
            }
        });
    }
    {
        let (a, p) = (app.clone(), panel.as_weak());
        panel.on_item_drop(move |id, _, _, _| {
            // defer: rebuilding the groups model inside the slot's own event handler is unsafe
            let (a, p) = (a.clone(), p.clone());
            slint::Timer::single_shot(Duration::ZERO, move || {
                if let Some(p) = p.upgrade() {
                    let to = drop_item(&p, &a, id);
                    land(&p, &a, to);
                }
            });
        });
    }
    {
        let (a, p) = (app.clone(), panel.as_weak());
        panel.on_group_drag(move |idx, x, y, resize| {
            if let Some(p) = p.upgrade() {
                // real cursor, grid coords (pad 16, header 48 as in app.slint)
                let (x, y) = plat::cursor_logical(p.window()).map_or((x, y), |(cx, cy)| (cx - 16.0, cy - 48.0));
                group_drag(&mut a.borrow_mut(), idx as usize, x, y, resize);
                show_preview(&p, &a.borrow(), true);
            }
        });
    }
    {
        let (a, p) = (app.clone(), panel.as_weak());
        panel.on_group_drop(move |idx, _, _, resize| {
            let (a, p) = (a.clone(), p.clone());
            slint::Timer::single_shot(Duration::ZERO, move || {
                if let Some(p) = p.upgrade() {
                    drop_group(&p, &a, idx as usize, resize);
                }
            });
        });
    }
    {
        let (a, p) = (app.clone(), panel.as_weak());
        panel.on_drag_cancel(move || {
            if let Some(p) = p.upgrade() {
                reset_preview(&mut a.borrow_mut());
                show_preview(&p, &a.borrow(), false);
                // a shortcut was let go outside: back to its slot
                let back = if p.get_land_on() { a.borrow().ids.get(p.get_land_id().max(0) as usize).cloned() } else { None };
                land(&p, &a, back);
            }
        });
    }
    {
        let (a, p) = (app.clone(), panel.as_weak());
        panel.on_rename_group(move |old, new| {
            let (a, p) = (a.clone(), p.clone());
            slint::Timer::single_shot(Duration::ZERO, move || {
                if let Some(p) = p.upgrade() {
                    rename_group(&p, &a, &old, &new);
                }
            });
        });
    }
    panel.on_quit(move || {
        give_back_icons();
        let _ = slint::quit_event_loop();
    });
    Ok((panel, app))
}

/// Pin / unpin the folder shown in this window. Pinned: window stays on the desktop, tile hides.
fn set_pin(panel: &Panel, app: &Shared, on: bool) {
    let dir = {
        let mut st = app.borrow_mut();
        let Some(dir) = st.folder.as_ref().map(|f| f.dir.clone()) else { return };
        st.pinned = on;
        dir
    };
    panel.set_pinned(on);
    plat::set_pinned(panel.window(), on);
    let mut cfg = model::load_cfg(&dir);
    cfg.pin = if on { plat::rect_of(panel.window()).map(|r| [r.left, r.top]) } else { None };
    if let Err(e) = model::save_cfg(&dir, &cfg) {
        eprintln!("glass-folders: save config in {}: {e}", dir.display());
    }
    show_tile(&dir, !on);
    // this window stays on the desktop: tiles open folders in a fresh one from now on
    let is_main = g().panels.borrow().first().is_some_and(|(_, a)| Rc::ptr_eq(a, app));
    if on && is_main {
        match make_panel() {
            Ok(n) => g().panels.borrow_mut().insert(0, n),
            Err(e) => eprintln!("glass-folders: new panel: {e}"),
        }
    }
}

/// Start: a pinned folder opens where it was left (no animation, no focus).
fn pin_open(panel: &Panel, app: &Shared, dir: &Path, x: i32, y: i32) {
    {
        let mut st = app.borrow_mut();
        st.folder = Some(model::scan(dir));
        st.pinned = true;
    }
    render(panel, app);
    let (w, h) = panel_size(panel);
    panel.window().set_size(PhysicalSize::new(w as u32, h as u32));
    // screen may have changed: keep it on the work area
    let wa = win::work_area();
    let x = x.clamp(wa.left, (wa.right - w).max(wa.left));
    let y = y.clamp(wa.top, (wa.bottom - h).max(wa.top));
    panel.window().set_position(PhysicalPosition::new(x, y));
    panel.set_pinned(true);
    plat::set_pinned(panel.window(), true);
    let _ = panel.show();
    panel.set_grow_anim(true);
    show_tile(dir, false);
}

fn show_tile(dir: &Path, on: bool) {
    let tiles = g().tiles.borrow();
    if let Some((_, t)) = tiles.iter().find(|(d, _)| d == dir) {
        let _ = if on { t.show() } else { t.hide() };
    }
}

/// Scale to fetch icons at. Large icon size: no bigger than plain DPI — above 48 px the shell
/// draws low-res icons inside a square frame; a slight upscale looks better.
fn icon_scale(win: &slint::Window) -> f32 {
    win.scale_factor() / g().set.borrow().zoom().max(1.0)
}

/// Panel window size (physical px) = same formula as Panel width/height in app.slint.
fn panel_size(panel: &Panel) -> (i32, i32) {
    let s = panel.window().scale_factor();
    let w = ((panel.get_grid_w() + 32.0).max(panel.get_min_w()) * s).round() as i32;
    let h = ((panel.get_grid_h() + 64.0) * s).round() as i32;
    (w, h)
}

// ---------- name hint ----------

/// Pointer rests on a shortcut (window coords x, y, w, h of it): its name shows under it after a moment.
/// None = pointer left: hide now. Moving between shortcuts while shown: follows at once.
fn hint_at(win: &slint::Window, text: Option<String>, x: f32, y: f32, w: f32, h: f32) {
    g().hint_timer.borrow_mut().take();
    let Some(text) = text.filter(|t| !t.is_empty()) else {
        hide_hint();
        return;
    };
    let Some(r) = plat::rect_of(win) else { return };
    // panel still growing out of its tile: content corner is not the window corner
    let o = POINT { x: r.left, y: r.top };
    let s = win.scale_factor();
    let item = RECT {
        left: o.x + px(x, s),
        top: o.y + px(y, s),
        right: o.x + px(x + w, s),
        bottom: o.y + px(y + h, s),
    };
    let show = move || {
        let hint = g().hint.borrow();
        let Some(hint) = hint.as_ref() else { return };
        hint.set_text(text.clone().into());
        let hs = hint.window().scale_factor();
        let (pw, ph) = (px(hint.get_pw(), hs), px(28.0, hs));
        let wa = win::work_area();
        let gap = px(4.0, hs);
        let cx = (item.left + item.right) / 2;
        let x = (cx - pw / 2).clamp(wa.left, (wa.right - pw).max(wa.left));
        let mut y = item.bottom + gap;
        if y + ph > wa.bottom {
            y = item.top - gap - ph; // no room below: above
        }
        hint.window().set_size(PhysicalSize::new(pw as u32, ph as u32));
        hint.window().set_position(PhysicalPosition::new(x, y));
        let _ = hint.show();
        g().hint_on.set(true);
    };
    if g().hint_on.get() {
        show();
    } else {
        let t = slint::Timer::default();
        t.start(slint::TimerMode::SingleShot, Duration::from_millis(HINT_MS), show);
        *g().hint_timer.borrow_mut() = Some(t);
    }
}

fn hide_hint() {
    g().hint_timer.borrow_mut().take();
    if g().hint_on.replace(false) {
        if let Some(h) = g().hint.borrow().as_ref() {
            let _ = h.hide();
        }
    }
}

/// Folder icons back to their spots; nothing left to restore next start.
fn give_back_icons() {
    desk::restore_folders(&HashMap::new());
    let mut s = g().set.borrow_mut();
    if !s.homes.is_empty() {
        s.homes.clear();
        model::save_settings(&s);
    }
}

fn push_settings(panel: &Panel, set: &model::Settings) {
    panel.set_labels(set.labels);
    panel.set_names(set.names);
    panel.set_dots(set.dots);
    panel.set_glass(set.glass);
    panel.set_gravity(set.gravity);
    panel.set_snap(set.snap);
    panel.set_ptitle(set.ptitle);
    panel.set_tnames(set.tnames);
    panel.set_isize(set.isize);
    panel.set_en(set.en());
}

/// One setting changed in the settings page (any folder window): applies to all windows + tiles.
fn set_opt(key: &str, v: i32) {
    let gl = g();
    let (relayout, snap_now, retile, rezoom) = {
        let mut s = gl.set.borrow_mut();
        let on = v != 0;
        let (mut relayout, mut snap_now, mut retile, mut rezoom) = (false, false, false, false);
        match key {
            "labels" => s.labels = on,
            "names" => (s.names, relayout) = (on, true),
            "dots" => (s.dots, relayout) = (on, true),
            "ptitle" => s.ptitle = on,
            "tnames" => (s.tnames, retile) = (on, true),
            "gravity" => (s.gravity, relayout) = (v.clamp(0, 2), true),
            "glass" => s.glass = v.clamp(0, 2),
            "snap" => (s.snap, snap_now) = (on, on),
            "isize" => (s.isize, rezoom) = (v.clamp(0, 2), v.clamp(0, 2) != s.isize),
            "lang" => s.lang = v.clamp(0, 1),
            _ => return,
        }
        model::save_settings(&s);
        plat::set_snap(s.snap);
        model::set_header(s.names || s.dots);
        (relayout, snap_now, retile, rezoom)
    };
    let s = gl.set.borrow().clone();
    let panels = all_panels();
    for (p, _) in &panels {
        push_settings(p, &s);
    }
    for (_, t) in gl.tiles.borrow().iter() {
        reblur(t, gl.dark.get(), s.glass);
    }
    if retile {
        // same cells, new cell shape: tile grows / shrinks by the name strip
        for (d, t) in gl.tiles.borrow().iter() {
            let Some(r) = plat::rect_of(t.window()) else { continue };
            let sc = t.window().scale_factor();
            plat::set_tile_named(!s.tnames); // measure the old way
            let (c, rows) = plat::tile_cells(r.right - r.left, r.bottom - r.top, sc);
            plat::set_tile_named(s.tnames);
            let (w, h) = plat::tile_size(c, rows, sc);
            t.set_named(s.tnames);
            t.window().set_size(PhysicalSize::new(w as u32, h as u32));
            let mut cfg = model::load_cfg(d);
            cfg.tile = Some([r.left, r.top, w, h]);
            let _ = model::save_cfg(d, &cfg);
        }
    }
    if snap_now {
        // grid switched on: every tile onto the grid now
        for (d, t) in gl.tiles.borrow().iter() {
            let Some(r) = plat::rect_of(t.window()) else { continue };
            let (x, y) = plat::snap_pos(r.left, r.top, t.window().scale_factor());
            t.window().set_position(PhysicalPosition::new(x, y));
            let mut cfg = model::load_cfg(d);
            cfg.tile = Some([x, y, r.right - r.left, r.bottom - r.top]);
            let _ = model::save_cfg(d, &cfg);
        }
    }
    if rezoom {
        // icon size = zoom: tiles keep their cells (grow / shrink with them), panels re-render
        let z = s.zoom();
        let dirs: Vec<PathBuf> = {
            let tiles = gl.tiles.borrow();
            for (d, t) in tiles.iter() {
                let Some(r) = plat::rect_of(t.window()) else { continue };
                let (c, rows) = plat::tile_cells(r.right - r.left, r.bottom - r.top, t.window().scale_factor());
                plat::set_zoom(t.window(), z);
                let (w, h) = plat::tile_size(c, rows, t.window().scale_factor());
                t.window().set_size(PhysicalSize::new(w as u32, h as u32));
                let mut cfg = model::load_cfg(d);
                cfg.tile = Some([r.left, r.top, w, h]);
                let _ = model::save_cfg(d, &cfg);
            }
            tiles.iter().map(|(d, _)| d.clone()).collect()
        };
        // only the new pixel sizes stay cached; the rest loads in the background
        let mut sizes = Vec::new();
        if let Some((p, _)) = main_panel() {
            sizes.push(px(48.0, icon_scale(p.window())));
        }
        if let Some((_, t)) = g().tiles.borrow().first() {
            sizes.push(px(36.0, icon_scale(t.window())));
        }
        icons::keep_sizes(&sizes);
        for d in dirs {
            refresh_tile(&d); // icons at the new pixel size
        }
        prefetch_icons();
        for (p, a) in &panels {
            plat::set_zoom(p.window(), z);
            if a.borrow().folder.is_some() {
                render(p, a);
                let (w, h) = panel_size(p);
                p.window().set_size(PhysicalSize::new(w as u32, h as u32));
                fit_panel(p);
            }
        }
    }
    if relayout {
        for (p, a) in &panels {
            if a.borrow().folder.is_some() {
                render(p, a);
            }
        }
    }
}

/// Desktop tiles: one per folder on Desktop. Place: saved rect; else where the folder icon stands
/// on the desktop (first run); else a column at the right edge. Never on top of another tile.
/// Returns home spots of the folder icons (to park them away while tiles stand in).
fn make_tiles(desktop: &Path, killed: &HashMap<PathBuf, POINT>) -> Result<HashMap<PathBuf, POINT>, slint::PlatformError> {
    let dark = g().dark.get();
    let (glass, named, zoom) = {
        let s = g().set.borrow();
        (s.glass, s.tnames, s.zoom())
    };
    let mut native = desk::icon_positions();
    // killed run: its saved spots win (icon may not be back on the desktop yet)
    for (k, p) in killed {
        native.insert(k.clone(), *p);
    }
    let mut homes = HashMap::new();
    // dev: GF_DUMP_ICONS=file writes where Explorer has each desktop icon
    if let Ok(out) = std::env::var("GF_DUMP_ICONS") {
        let txt: String = native.iter().map(|(p, pt)| format!("{}\t{}\t{}\n", p.display(), pt.x, pt.y)).collect();
        let _ = std::fs::write(out, txt);
    }
    let folders: Vec<model::Folder> = model::list_folders(desktop).iter().map(|d| model::scan(d)).collect();
    let wa = win::work_area();
    let mut taken: Vec<RECT> = folders
        .iter()
        .filter_map(|f| f.cfg.tile)
        .map(|[x, y, w, h]| RECT { left: x, top: y, right: x + w, bottom: y + h })
        .collect();
    let dev_row = std::env::var("GF_TILE_AT")
        .ok()
        .and_then(|v| v.split_once(',').map(|(a, b)| (a.parse::<i32>().unwrap_or(0), b.parse::<i32>().unwrap_or(0))));

    for (i, f) in folders.into_iter().enumerate() {
        let dir = f.dir.clone();
        let t = Tile::new()?;
        plat::set_kind(t.window(), plat::Kind::Tile);
        plat::set_zoom(t.window(), zoom);
        t.set_dark(dark);
        t.set_name(f.name.clone().into());
        t.set_named(named);
        let (c, on) = model::tint(&f.tint());
        t.set_tint(c);
        t.set_colored(on);
        let items = tile_icons(&t, &f);
        g().tile_items.borrow_mut().insert(dir.clone(), items);
        {
            let d = dir.clone();
            t.on_open(move || {
                hide_hint();
                if let Some((p, a)) = main_panel() {
                    open_folder(&p, &a, &d);
                }
            });
        }
        {
            // click on a tile icon: run it right from the desktop, no panel
            let d = dir.clone();
            t.on_launch(move |i| {
                hide_hint();
                let path = g().tile_items.borrow().get(&d).and_then(|v| v.get(i as usize).cloned());
                let Some(path) = path else { return };
                win::open(&path);
                model::bump_use(&d, &path);
                // deferred: icons model is rebuilt, the clicked icon belongs to it
                let d = d.clone();
                slint::Timer::single_shot(Duration::ZERO, move || refresh_tile(&d));
            });
        }
        {
            // pointer rests on a tile icon: its name
            let (d, tw) = (dir.clone(), t.as_weak());
            t.on_hover(move |i, x, y, w, h| {
                let name = if i < 0 { None } else { g().tile_items.borrow().get(&d).and_then(|v| v.get(i as usize)).map(|f| model::name_of(f)) };
                if let Some(t) = tw.upgrade() {
                    hint_at(t.window(), name, x, y, w, h);
                }
            });
        }
        if let Some(h) = plat::hwnd_of(t.window()) {
            win::glass(h, dark);
        }
        let s = t.window().scale_factor();
        let (side_w, side_h) = plat::snap_size(0, 0, s); // min tile = 2x2 cells
        let (x, y, w, h) = if let Some((ax, ay)) = dev_row {
            // dev: GF_TILE_AT=x,y puts tiles in a row from that point (free, not snapped)
            (ax + i as i32 * (side_w + 16), ay, side_w, side_h)
        } else if let Some([sx, sy, sw, sh]) = f.cfg.tile {
            let (w, h) = plat::snap_size(sw, sh, s);
            // grid off: stays exactly where the user left it
            let (x, y) = if g().set.borrow().snap { plat::snap_pos(sx, sy, s) } else { (sx, sy) };
            (x, y, w, h)
        } else {
            let start = match native.get(&dir) {
                Some(p) => (p.x, p.y),
                None => (wa.right - ((24.0 + 112.0) * s) as i32, wa.top + (24.0 * s) as i32),
            };
            let (x, y) = free_spot(start, side_w, side_h, &taken, s);
            // remember right away: next start puts it in the same place
            let mut cfg = f.cfg.clone();
            cfg.tile = Some([x, y, side_w, side_h]);
            let _ = model::save_cfg(&dir, &cfg);
            (x, y, side_w, side_h)
        };
        taken.push(RECT { left: x, top: y, right: x + w, bottom: y + h });
        if let Some(p) = native.get(&dir) {
            homes.insert(dir.clone(), *p);
        }
        t.window().set_size(PhysicalSize::new(w as u32, h as u32));
        t.window().set_position(PhysicalPosition::new(x, y));
        reblur(&t, dark, glass);
        {
            // user moved/resized the tile: remember it, re-blur what is under it now
            let (tw, d) = (t.as_weak(), dir.clone());
            plat::set_on_moved(t.window(), move |r| {
                let mut cfg = model::load_cfg(&d);
                cfg.tile = Some([r.left, r.top, r.right - r.left, r.bottom - r.top]);
                let _ = model::save_cfg(&d, &cfg);
                if let Some(t) = tw.upgrade() {
                    reblur(&t, g().dark.get(), g().set.borrow().glass);
                }
            });
        }
        // pinned folder: its open window stands in for the tile (shown by pin_open, not here)
        if f.cfg.pin.is_none() {
            t.show()?;
        }
        g().tiles.borrow_mut().push((dir, t));
    }
    Ok(homes)
}

/// Snapped spot near `start` (physical px) where a w x h tile hits no taken rect.
/// Walks down the column, then the next column to the right; stays inside the work area.
fn free_spot(start: (i32, i32), w: i32, h: i32, taken: &[RECT], s: f32) -> (i32, i32) {
    let wa = win::work_area();
    let (cw, ch) = ((plat::CELL_W * s).round() as i32, (plat::CELL_H * s).round() as i32);
    let clamp = |x: i32, y: i32| plat::snap_pos(x.clamp(wa.left, (wa.right - w).max(wa.left)), y.clamp(wa.top, (wa.bottom - h).max(wa.top)), s);
    let (x0, y0) = clamp(start.0, start.1);
    let free = |x: i32, y: i32| !taken.iter().any(|r| x < r.right && r.left < x + w && y < r.bottom && r.top < y + h);
    let mut x = x0;
    for _ in 0..200 {
        let mut y = y0;
        while y + h <= wa.bottom {
            if free(x, y) {
                return (x, y);
            }
            y += ch;
        }
        x += cw;
        if x + w > wa.right {
            x = wa.left; // wrap to the left edge
        }
    }
    (x0, y0)
}

/// Tile icons: most launched first. Enough for a big tile; Slint shows only what fits.
/// Returns the files behind the icons (same order) for launch from the tile.
fn tile_icons(t: &Tile, f: &model::Folder) -> Vec<PathBuf> {
    let all = f.top_items();
    t.set_total(all.len() as i32);
    let cand: Vec<PathBuf> = all.into_iter().take(24).collect();
    let (items, done) = set_tile_icons(t, &cand);
    let mut wait = g().tile_wait.borrow_mut();
    if done {
        wait.remove(&f.dir);
    } else {
        wait.insert(f.dir.clone(), cand);
    }
    items
}

/// Tile icons from the cache; missing ones queued and shown empty (in place) until they land.
/// Files the shell has no icon for are left out. Returns (files behind the icons, all loaded).
fn set_tile_icons(t: &Tile, cand: &[PathBuf]) -> (Vec<PathBuf>, bool) {
    let ipx = px(36.0, icon_scale(t.window()));
    let mut missing = Vec::new();
    let mut items = Vec::new();
    let mut imgs = Vec::new();
    for p in cand {
        match icons::get(p, ipx) {
            Some(i) if i.size().width == 0 => continue,
            Some(i) => imgs.push(i),
            None => {
                missing.push(p.clone());
                imgs.push(Image::default());
            }
        }
        items.push(p.clone());
    }
    let done = missing.is_empty();
    icons::want(missing, ipx, true);
    t.set_icons(ModelRc::new(VecModel::from(imgs)));
    (items, done)
}

/// New icons arrived: fill the empty slots of open panels and of tiles still waiting.
fn icons_ready() {
    for (p, a) in all_panels() {
        patch_icons(&p, &a);
    }
    let wait: Vec<(PathBuf, Vec<PathBuf>)> = g().tile_wait.borrow().iter().map(|(d, c)| (d.clone(), c.clone())).collect();
    for (dir, cand) in wait {
        let Some(t) = g().tiles.borrow().iter().find(|(d, _)| *d == dir).map(|(_, t)| t.clone_strong()) else { continue };
        let (items, done) = set_tile_icons(&t, &cand);
        g().tile_items.borrow_mut().insert(dir.clone(), items);
        if done {
            g().tile_wait.borrow_mut().remove(&dir);
        }
    }
}

/// Put freshly loaded icons into the panel's slots, in place (no relayout).
fn patch_icons(panel: &Panel, app: &Shared) {
    let st = app.borrow();
    let Some(groups) = st.groups.as_ref() else { return };
    let ipx = px(48.0, icon_scale(panel.window()));
    for gi in 0..groups.row_count() {
        let Some(gd) = groups.row_data(gi) else { continue };
        for k in 0..gd.items.row_count() {
            let Some(mut s) = gd.items.row_data(k) else { continue };
            if s.icon.size().width > 0 {
                continue;
            }
            let Some(path) = st.ids.get(s.id as usize) else { continue };
            if let Some(i) = icons::get(path, ipx).filter(|i| i.size().width > 0) {
                s.icon = i;
                gd.items.set_row_data(k, s);
            }
        }
    }
}

/// Re-read one folder from disk and redraw its desktop tile (launch counts changed).
fn refresh_tile(dir: &Path) {
    let f = model::scan(dir);
    let items = g().tiles.borrow().iter().find(|(d, _)| d == dir).map(|(_, t)| tile_icons(t, &f));
    if let Some(items) = items {
        g().tile_items.borrow_mut().insert(dir.to_path_buf(), items);
    }
}

// ---------- panel drag: preview ----------

fn hit(r: &GRect, x: f32, y: f32, pad: f32) -> bool {
    let [rx, ry, rw, rh] = r.px();
    x >= rx - pad && x <= rx + rw + pad && y >= ry - pad && y <= ry + rh + pad
}

fn reset_preview(st: &mut App) {
    st.prev = st.base.clone();
    st.ghost = None;
    st.target = Target::None;
    st.grab = None;
}

/// Dragged shortcut at grid point (x, y): pick target group + place, or a new-group plate
/// right under the cursor (groups below make room).
fn item_drag(st: &mut App, id: i32, x: f32, y: f32, inside: bool) {
    let Some(&src) = st.id_group.get(id as usize) else { return };
    if !inside {
        reset_preview(st);
        return;
    }
    // still over the plate: keep it (no jitter)
    if st.ghost.is_some_and(|g| hit(&g, x, y, GAP / 2.0)) {
        return;
    }
    // over a group (as shown now; gaps count as the nearer group)
    if let Some(i) = st.prev.iter().position(|r| hit(r, x, y, GAP / 2.0)) {
        // group at its own place: undo pushes left by a plate the cursor passed over
        if hit(&st.base[i], x, y, GAP / 2.0) {
            st.prev = st.base.clone();
        }
        let r = st.prev[i];
        let [rx, ry, _, _] = r.px();
        let Some(f) = st.folder.as_ref() else { return };
        let col = ((x - rx) / PITCH).floor().clamp(0.0, (r.w - 1) as f32) as usize;
        let row = ((y - ry - st.head()) / PITCH).floor().max(0.0) as usize;
        let n = f.groups[i].items.len() - usize::from(i == src); // items besides the dragged one
        let cap = (r.w * r.rows()) as usize;
        let k = (row * r.w as usize + col).min(n).min(cap.saturating_sub(1));
        st.ghost = None; // groups stay as shown: what you see is where you drop
        st.target = Target::Group(i, k);
        return;
    }
    // empty space: 1x1 plate with its slot under the cursor, never over a group above it
    let mut g = GRect::slots(1, 1, (x / PITCH).floor().max(0.0) as i32, ((y - st.head() - model::CELL / 2.0) / ROW).round().max(0.0) as i32);
    while let Some(b) = st.base.iter().filter(|r| r.hits(&g) && r.y < g.y).map(|r| r.y + r.h).max() {
        g.y = b;
    }
    let mut all = st.base.clone();
    all.push(g);
    let last = all.len() - 1;
    model::resolve(&mut all, Some(last));
    model::settle(&mut all, crate::g().set.borrow().gravity); // local `g` = plate
    st.ghost = all.pop();
    st.prev = all;
    st.target = Target::New;
}

/// Group header / grip dragged to grid point (x, y): move or resize it; others make room.
fn group_drag(st: &mut App, idx: usize, x: f32, y: f32, resize: bool) {
    let Some(mut r) = st.base.get(idx).copied() else { return };
    let [rx, ry, rw, rh] = r.px();
    if resize {
        let w = ((x - rx + GAP) / PITCH).round().max(1.0) as i32;
        let rows = ((y - ry - model::header_px() + GAP) / PITCH).round().max(1.0) as i32;
        r = GRect::slots(w, rows, r.x, r.y);
        // plate follows the cursor, not smaller than one slot
        st.lift = [rx, ry, (x - rx).max(model::CELL), (y - ry).max(model::CELL + model::header_px() - GAP)];
    } else {
        let (ox, oy) = *st.grab.get_or_insert((x - rx, y - ry));
        r.x = ((x - ox) / PITCH).round().max(0.0) as i32;
        r.y = ((y - oy) / ROW).round().max(0.0) as i32;
        st.lift = [(x - ox).max(0.0), (y - oy).max(0.0), rw, rh];
    }
    let mut all = st.base.clone();
    all[idx] = r;
    model::resolve(&mut all, Some(idx));
    model::settle(&mut all, g().set.borrow().gravity);
    st.prev = all;
}

/// Push preview into the panel: group rects (rows keep their item models, so a drag in progress
/// is not broken), plate, drop target, canvas size (room to drop right / below while dragging).
fn show_preview(panel: &Panel, st: &App, dragging: bool) {
    if let Some(m) = st.groups.as_ref() {
        for (i, r) in st.prev.iter().enumerate() {
            let Some(mut d) = m.row_data(i) else { continue };
            let [x, y, w, h] = r.px();
            if (d.x, d.y, d.w, d.h) != (x, y, w, h) {
                (d.x, d.y, d.w, d.h, d.cols, d.rows) = (x, y, w, h, r.w, r.rows());
                m.set_row_data(i, d);
            }
        }
    }
    let (t, k) = match st.target {
        Target::None => (-2, 0),
        Target::Group(i, k) => (i as i32, k as i32),
        Target::New => (-1, 0),
    };
    panel.set_drop_target(t);
    panel.set_drop_index(k);
    let [lx, ly, lw, lh] = st.lift;
    panel.set_lift_x(lx);
    panel.set_lift_y(ly);
    panel.set_lift_w(lw);
    panel.set_lift_h(lh);
    if let Some(g) = st.ghost {
        let [x, y, w, h] = g.px();
        panel.set_ghost_x(x);
        panel.set_ghost_y(y);
        panel.set_ghost_w(w);
        panel.set_ghost_h(h);
    }
    panel.set_ghost_on(st.ghost.is_some());
    let mut all: Vec<GRect> = st.base.iter().chain(&st.prev).copied().chain(st.ghost).collect();
    if all.is_empty() {
        all.push(GRect::slots(1, 1, 0, 0));
    }
    let (w, h) = if dragging { model::grid_px(&all, 1, 3) } else { model::grid_px(&st.base, 0, 0) };
    panel.set_grid_w(w);
    panel.set_grid_h(h);
}

// ---------- panel drag: drop ----------

/// Save shown group places (prev) into cfg, by group name.
fn keep_places(cfg: &mut model::Config, f: &model::Folder, prev: &[GRect]) {
    for (g, r) in f.groups.iter().zip(prev) {
        let c = cfg.groups.entry(g.name.clone()).or_default();
        (c.x, c.y) = (Some(r.x), Some(r.y));
    }
}

fn names(items: &[PathBuf]) -> Vec<String> {
    items.iter().filter_map(|p| model::file_name(p)).collect()
}

/// Drop dragged shortcut: reorder in its group, move file into another group (real fs move),
/// or create a new group folder where the plate is. Any fs error: logged, item stays.
/// Drop a dragged shortcut. Returns the file where it ended up (for the landing glide), None = gone.
fn drop_item(panel: &Panel, app: &Shared, id: i32) -> Option<PathBuf> {
    let (root, created, landed) = {
        let mut st = app.borrow_mut();
        let target = st.target;
        let st = &mut *st;
        if target == Target::None {
            reset_preview(st);
            show_preview(panel, st, false);
            return st.ids.get(id as usize).cloned(); // back to its own slot
        }
        let (Some(f), Some(src), Some(&sg)) = (st.folder.as_ref(), st.ids.get(id as usize), st.id_group.get(id as usize)) else { return None };
        let root = f.dir.clone();
        let src_name = model::file_name(src).unwrap_or_default();
        let mut cfg = model::load_cfg(&root); // fresh from disk: tile rect may have changed since open
        let mut created = None;
        let mut landed = src.clone();
        match target {
            Target::Group(t, k) if t == sg => {
                let mut order = names(&f.groups[t].items);
                order.retain(|n| *n != src_name);
                order.insert(k.min(order.len()), src_name);
                cfg.groups.entry(f.groups[t].name.clone()).or_default().items = order;
                keep_places(&mut cfg, f, &st.prev);
            }
            _ => {
                let src_dir = f.groups[sg].dir.clone();
                let (dst, k, new_name) = match target {
                    Target::Group(t, k) => (f.groups[t].dir.clone(), k, None),
                    _ => match model::create_group(&root, g().set.borrow().en()) {
                        Ok(n) => (root.join(&n), 0, Some(n)),
                        Err(e) => {
                            eprintln!("glass-folders: create group in {}: {e}", root.display());
                            return None;
                        }
                    },
                };
                let moved = match model::move_into(src, &dst) {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("glass-folders: move {} -> {}: {e}", src.display(), dst.display());
                        if new_name.is_some() {
                            model::remove_if_empty(&dst);
                        }
                        return None;
                    }
                };
                landed = moved.clone();
                keep_places(&mut cfg, f, &st.prev);
                // target order: new name at drop place
                if let Target::Group(t, _) = target {
                    let mut order = names(&f.groups[t].items);
                    order.insert(k.min(order.len()), model::file_name(&moved).unwrap_or_default());
                    cfg.groups.entry(f.groups[t].name.clone()).or_default().items = order;
                }
                // source order: without it
                let sname = f.groups[sg].name.clone();
                let mut sorder = names(&f.groups[sg].items);
                sorder.retain(|n| *n != src_name);
                cfg.groups.entry(sname.clone()).or_default().items = sorder;
                // emptied subfolder: removed, settings forgotten
                if src_dir != root && model::remove_if_empty(&src_dir) {
                    cfg.forget_group(&sname);
                }
                if let (Some(n), Some(g)) = (&new_name, st.ghost) {
                    cfg.forget_group(n); // stale settings of an old group with the same name
                    let c = cfg.groups.entry(n.clone()).or_default();
                    (c.x, c.y) = (Some(g.x), Some(g.y));
                }
                created = new_name;
            }
        }
        if let Err(e) = model::save_cfg(&root, &cfg) {
            eprintln!("glass-folders: save config in {}: {e}", root.display());
        }
        (root, created, landed)
    };
    rescan(panel, app, &root);
    if let Some(n) = created {
        let idx = app.borrow().folder.as_ref().and_then(|f| f.groups.iter().position(|g| g.name == n));
        if let Some(i) = idx {
            panel.set_rename_idx(i as i32);
        }
    }
    Some(landed)
}

/// Released shortcut glides from the cursor into its slot (Slint put land-* at the cursor;
/// here: target point + the slot kept hidden meanwhile). path None = no landing.
fn land(panel: &Panel, app: &Shared, path: Option<PathBuf>) {
    const LAND_MS: u64 = 190;
    let spot = path.and_then(|path| {
        let st = app.borrow();
        let f = st.folder.as_ref()?;
        let id = st.ids.iter().position(|p| *p == path)?;
        let gi = *st.id_group.get(id)?;
        let items = &f.groups.get(gi)?.items;
        let r = *st.base.get(gi)?;
        let mut k = items.iter().position(|p| *p == path)? as i32;
        let cap = r.w * r.rows();
        let mut hide = id as i32;
        if st.unfolded != Some(gi) && items.len() as i32 > cap && k >= cap - 1 {
            k = cap - 1; // behind "+N": fly into that slot, it stays as is
            hide = -1;
        }
        let [gx, gy, _, _] = r.px();
        let icon_top = if g().set.borrow().labels { 6.0 } else { 14.0 };
        let x = 16.0 + gx + (k % r.w) as f32 * PITCH + 38.0;
        let y = 48.0 + gy + st.head() + (k / r.w) as f32 * PITCH + icon_top + 24.0;
        Some((hide, x, y))
    });
    let Some((hide, x, y)) = spot else {
        panel.set_land_on(false);
        return;
    };
    panel.set_land_id(hide);
    panel.set_land_go(true);
    panel.set_land_x(x);
    panel.set_land_y(y);
    let p = panel.as_weak();
    slint::Timer::single_shot(Duration::from_millis(LAND_MS), move || {
        if let Some(p) = p.upgrade() {
            p.set_land_on(false);
            p.set_land_go(false);
            p.set_land_id(-1);
        }
    });
}

/// Group moved / resized: save all shown places (others may have made room) + new size.
fn drop_group(panel: &Panel, app: &Shared, idx: usize, resize: bool) {
    let root = {
        let st = app.borrow();
        let Some(f) = st.folder.as_ref() else { return };
        let mut cfg = model::load_cfg(&f.dir);
        keep_places(&mut cfg, f, &st.prev);
        if let (true, Some(g), Some(r)) = (resize, f.groups.get(idx), st.prev.get(idx)) {
            let c = cfg.groups.entry(g.name.clone()).or_default();
            (c.w, c.h) = (Some(r.w as u32), Some(r.rows() as u32));
        }
        if let Err(e) = model::save_cfg(&f.dir, &cfg) {
            eprintln!("glass-folders: save config in {}: {e}", f.dir.display());
        }
        f.dir.clone()
    };
    rescan(panel, app, &root);
}

/// Inline rename committed (Enter). Empty / unchanged = keep name.
fn rename_group(panel: &Panel, app: &Shared, old: &str, typed: &str) {
    panel.set_rename_idx(-1);
    let name = model::clean_name(typed);
    let root = app.borrow().folder.as_ref().map(|f| f.dir.clone());
    if let (Some(root), false) = (root, name.is_empty() || name == old || old == model::LOOSE) {
        match model::rename_group(&root, old, &name) {
            Ok(n) => {
                let mut cfg = model::load_cfg(&root);
                if cfg.groups.contains_key(old) || cfg.order.iter().any(|o| o == old) {
                    cfg.rename_group(old, &n);
                    if let Err(e) = model::save_cfg(&root, &cfg) {
                        eprintln!("glass-folders: save config in {}: {e}", root.display());
                    }
                }
            }
            Err(e) => eprintln!("glass-folders: rename group {old} -> {name}: {e}"),
        }
        rescan(panel, app, &root);
    }
    panel.invoke_focus_keys();
}

/// Folder changed on disk: re-read, redraw panel and the folder's desktop tile.
fn rescan(panel: &Panel, app: &Shared, root: &Path) {
    app.borrow_mut().folder = Some(model::scan(root));
    render(panel, app);
    refresh_tile(root);
}

/// Tile glass look. Default: real live acrylic (win::glass + NC-active trick), only a matte layer
/// by glass level. Env GF_FAKE_GLASS=1: old fallback, frosted copy of wallpaper under the tile.
fn reblur(t: &Tile, dark: bool, glass: i32) {
    t.set_glass(glass);
    if std::env::var("GF_FAKE_GLASS").is_err() {
        return;
    }
    let p = t.window().position();
    let sz = t.window().size();
    let r = RECT { left: p.x, top: p.y, right: p.x + sz.width as i32, bottom: p.y + sz.height as i32 };
    if let Some(img) = win::wallpaper_blur(r, dark, glass) {
        t.set_backdrop(img);
    }
}

/// Folder (idx < 0) or group color. None = colorless.
fn set_color(panel: &Panel, app: &Shared, idx: i32, hx: Option<String>) {
    {
        let mut st = app.borrow_mut();
        let st = &mut *st;
        let Some(f) = st.folder.as_mut() else { return };
        if idx < 0 {
            f.cfg.color = hx;
        } else if let Some(g) = f.groups.get(idx as usize) {
            let name = g.name.clone();
            f.cfg.groups.entry(name).or_default().color = hx;
        }
        // merge into fresh cfg: tile rect may have changed since open
        let mut cfg = model::load_cfg(&f.dir);
        cfg.color = f.cfg.color.clone();
        for (n, g) in &f.cfg.groups {
            cfg.groups.entry(n.clone()).or_default().color = g.color.clone();
        }
        let _ = model::save_cfg(&f.dir, &cfg);
        let (dir, (c, on)) = (f.dir.clone(), model::tint(&f.tint()));
        for (d, t) in g().tiles.borrow().iter() {
            if *d == dir {
                t.set_tint(c);
                t.set_colored(on);
            }
        }
    }
    render(panel, app);
}

fn open_folder(panel: &Panel, app: &Shared, dir: &Path) {
    // a fold-back still running (click on another tile closed the panel): finish it now
    finish_close(panel, app);
    let t0 = std::time::Instant::now();
    {
        let mut st = app.borrow_mut();
        st.folder = Some(model::scan(dir));
        st.unfolded = None;
    }
    plat::perf(|| format!("open {:?}: scan {:.1}ms", dir.file_name().unwrap_or_default(), t0.elapsed().as_secs_f64() * 1000.0));
    panel.set_grow_anim(false); // new folder: window takes its size at once
    panel.set_page(0);
    panel.set_pinned(false);
    render(panel, app);
    plat::perf(|| format!("open: +render {:.1}ms", t0.elapsed().as_secs_f64() * 1000.0));
    let s = panel.window().scale_factor();
    let (w, h) = panel_size(panel);
    panel.window().set_size(PhysicalSize::new(w as u32, h as u32));
    // place: centered on the tile, kept inside the work area
    let wa = win::work_area();
    let m = (8.0 * s) as i32;
    let from = app.borrow().tile_rect(dir).unwrap_or_else(|| {
        let (cx, cy) = ((wa.left + wa.right) / 2, (wa.top + wa.bottom) / 2);
        RECT { left: cx - 40, top: cy - 40, right: cx + 40, bottom: cy + 40 }
    });
    let (cx, cy) = ((from.left + from.right) / 2, (from.top + from.bottom) / 2);
    let x = (cx - w / 2).clamp(wa.left + m, (wa.right - w - m).max(wa.left + m));
    let y = (cy - h / 2).clamp(wa.top + m, (wa.bottom - h - m).max(wa.top + m));
    let to = RECT { left: x, top: y, right: x + w, bottom: y + h };
    {
        let mut st = app.borrow_mut();
        st.geo = Some((from, to));
        st.open_p = 0.0;
    }
    panel.set_reveal(1.0);
    plat::set_zoom_view(panel.window(), zoom_rect(from, to, 0.0), 0.0);
    let _ = panel.show();
    panel.invoke_focus_keys();
    animate(panel, app, true);
}

/// Window rect at openness p: the panel's own shape, scaled down onto the tile (p = 0) .. full size
/// in its place (p = 1). Same aspect all the way -> content scales evenly, nothing is cut off.
fn zoom_rect(tile: RECT, panel: RECT, p: f32) -> RECT {
    let (pw, ph) = ((panel.right - panel.left) as f32, (panel.bottom - panel.top) as f32);
    let (tw, th) = ((tile.right - tile.left) as f32, (tile.bottom - tile.top) as f32);
    let s0 = (tw / pw).max(th / ph).clamp(0.15, 1.0);
    let s = s0 + (1.0 - s0) * p;
    let mid = |a: i32, b: i32| (a + b) as f32 / 2.0;
    let cx = mid(tile.left, tile.right) + (mid(panel.left, panel.right) - mid(tile.left, tile.right)) * p;
    let cy = mid(tile.top, tile.bottom) + (mid(panel.top, panel.bottom) - mid(tile.top, tile.bottom)) * p;
    let (w, h) = ((pw * s).round() as i32, (ph * s).round() as i32);
    let (l, t) = ((cx - w as f32 / 2.0).round() as i32, (cy - h as f32 / 2.0).round() as i32);
    RECT { left: l, top: t, right: l + w, bottom: t + h }
}

/// Panel zooms out of its tile (opening) or back into it: openness glides to 1 / 0 from where it is
/// (a close during the open turns around smoothly). Content fades in early, out late.
fn animate(panel: &Panel, app: &Shared, opening: bool) {
    let start = std::time::Instant::now();
    let (p0, geo) = {
        let st = app.borrow();
        (st.open_p, st.geo)
    };
    let Some((tile, full)) = geo else { return };
    let target = if opening { 1.0 } else { 0.0 };
    let ms = (if opening { OPEN_MS } else { CLOSE_MS }) * (target - p0 as f32).abs().max(0.2);
    let (a, p) = (app.clone(), panel.as_weak());
    let t = slint::Timer::default();
    t.start(slint::TimerMode::Repeated, plat::FRAME, move || {
        let Some(p) = p.upgrade() else { return };
        if a.borrow().anim.is_none() {
            return; // stopped
        }
        let k = (start.elapsed().as_secs_f32() * 1000.0 / ms).min(1.0);
        plat::perf(|| format!("anim k={k:.3}"));
        // open: strong ease-out (quick start, long soft landing); close: smoothstep (lands on the tile
        // softly; plain ease-in vanished at half size = pop)
        let e = if opening { 1.0 - (1.0 - k).powi(4) } else { k * k * (3.0 - 2.0 * k) };
        let o = p0 + (target - p0) * e;
        a.borrow_mut().open_p = o;
        plat::set_zoom_view(p.window(), zoom_rect(tile, full, o), (o / 0.55).min(1.0));
        if k >= 1.0 {
            if let Some(t) = a.borrow_mut().anim.take() {
                slint::Timer::single_shot(Duration::ZERO, move || drop(t));
            }
            if opening {
                plat::end_zoom_view(p.window(), Some(POINT { x: full.left, y: full.top }));
                p.set_grow_anim(true);
            } else {
                finish_close(&p, &a);
            }
        }
    });
    app.borrow_mut().anim = Some(t);
}

/// Start folding the panel back into its tile; hidden + cleared at the end.
fn close(panel: &Panel, app: &Shared) {
    {
        let mut st = app.borrow_mut();
        if st.closing || st.folder.is_none() {
            return;
        }
        st.closing = true;
        st.anim.take(); // an unfinished open: turns around from where it is
        panel.set_grow_anim(false);
        let Some(cur) = plat::rect_of(panel.window()) else { return };
        // fully open: the panel may have been moved / resized since -> fold from where it is now
        let full = if plat::zoom_view_on(panel.window()) { st.geo.map(|g| g.1).unwrap_or(cur) } else { cur };
        let dir = st.folder.as_ref().map(|f| f.dir.clone()).unwrap_or_default();
        let tile = st.tile_rect(&dir).unwrap_or_else(|| {
            let (cx, cy) = ((full.left + full.right) / 2, (full.top + full.bottom) / 2);
            RECT { left: cx - 40, top: cy - 40, right: cx + 40, bottom: cy + 40 }
        });
        if !plat::zoom_view_on(panel.window()) {
            st.open_p = 1.0;
        }
        st.geo = Some((tile, full));
    }
    animate(panel, app, false);
}

/// Hide panel and free everything (end of fold-back, or cut short by a new open).
fn finish_close(panel: &Panel, app: &Shared) {
    {
        let mut st = app.borrow_mut();
        st.anim.take();
        if !st.closing {
            if st.folder.is_some() {
                // open still running: just land it
                let home = st.geo.map(|g| POINT { x: g.1.left, y: g.1.top });
                st.open_p = 1.0;
                drop(st);
                plat::end_zoom_view(panel.window(), home);
                panel.set_grow_anim(true);
            }
            return;
        }
        st.closing = false;
    }
    let _ = panel.hide();
    plat::end_zoom_view(panel.window(), None);
    panel.set_groups(ModelRc::default());
    panel.set_rename_idx(-1);
    panel.set_drop_target(-2);
    panel.set_ghost_on(false);
    let mut st = app.borrow_mut();
    st.folder = None;
    st.ids.clear();
    st.id_group.clear();
    st.base.clear();
    st.groups = None;
    st.unfolded = None;
    reset_preview(&mut st);
    drop(st);
    hide_hint();
    // closed window that is not the tiles' one (was pinned): gone for good
    let me = app.clone();
    slint::Timer::single_shot(Duration::ZERO, move || {
        let mut ps = g().panels.borrow_mut();
        let gone = ps.iter().position(|(_, a)| Rc::ptr_eq(a, &me)).filter(|&i| i > 0).map(|i| ps.remove(i));
        drop(ps);
        drop(gone);
    });
}

/// Push folder data + layout into the panel. All items go in (overflow "+N" is drawn by Slint,
/// so resizing a group shows more / fewer at once).
fn render(panel: &Panel, app: &Shared) {
    let mut st = app.borrow_mut();
    let st = &mut *st;
    let Some(f) = st.folder.as_ref() else { return };
    let gravity = g().set.borrow().gravity;
    let mut rects = model::layout(f, gravity);
    // unfolded group: only while it really overflows
    let unfolded = st.unfolded.filter(|&i| f.groups.get(i).is_some_and(|g| (g.items.len() as i32) > rects[i].w * rects[i].rows()));
    st.unfolded = unfolded;
    if let Some(i) = unfolded {
        // all items + fold slot; wider if it would get very tall
        let n = f.groups[i].items.len() as i32 + 1;
        let w = rects[i].w.max((n + 5) / 6);
        rects[i] = GRect::slots(w, (n + w - 1) / w, rects[i].x, rects[i].y);
        model::resolve(&mut rects, Some(i));
        model::settle(&mut rects, gravity);
    }
    let icon_px = px(48.0, icon_scale(panel.window()));
    st.ids.clear();
    st.id_group.clear();
    let mut missing = Vec::new();
    let mut groups = Vec::new();
    for (gi, (g, r)) in f.groups.iter().zip(&rects).enumerate() {
        let mut slots = Vec::new();
        for (k, path) in g.items.iter().enumerate() {
            let id = st.ids.len() as i32;
            st.ids.push(path.clone());
            st.id_group.push(gi);
            // not loaded yet: empty now (Slot fades it in), patched by patch_icons when it lands
            let icon = icons::get(path, icon_px).unwrap_or_else(|| {
                missing.push(path.clone());
                Image::default()
            });
            slots.push(SlotData { name: model::name_of(path).into(), icon, id, k: k as i32 });
        }
        let (tint, colored) = model::tint(&f.group_tint(gi));
        let [x, y, w, h] = r.px();
        groups.push(GroupData {
            name: g.name.clone().into(),
            tint,
            colored,
            fixed: g.dir == f.dir,
            idx: gi as i32,
            x,
            y,
            w,
            h,
            cols: r.w,
            rows: r.rows(),
            n: g.items.len() as i32,
            open: unfolded == Some(gi),
            items: ModelRc::new(VecModel::from(slots)),
        });
    }
    icons::want(missing, icon_px, true);
    panel.set_folder_name(f.name.clone().into());
    let (c, on) = model::tint(&f.tint());
    panel.set_folder_tint(c);
    panel.set_folder_colored(on);
    st.base = rects;
    reset_preview(st);
    let (gw, gh) = model::grid_px(&st.base, 0, 0);
    panel.set_grid_w(gw);
    panel.set_grid_h(gh);
    panel.set_drop_target(-2);
    panel.set_ghost_on(false);
    // same groups: update rows in place (plates glide to new places); else new model
    if let Some(m) = st.groups.as_ref().filter(|m| m.row_count() == groups.len()) {
        for (i, g) in groups.into_iter().enumerate() {
            m.set_row_data(i, g);
        }
    } else {
        let m = Rc::new(VecModel::from(groups));
        panel.set_groups(ModelRc::from(m.clone()));
        st.groups = Some(m);
    }
}

/// Panel grew (unfolded group): keep it inside the work area.
fn fit_panel(panel: &Panel) {
    let s = panel.window().scale_factor();
    let w = ((panel.get_grid_w() + 32.0).max(panel.get_min_w()) * s).round() as i32;
    let h = ((panel.get_grid_h() + 64.0) * s).round() as i32;
    let p = panel.window().position();
    let wa = win::work_area();
    let m = (8.0 * s) as i32;
    let x = p.x.min(wa.right - w - m).max(wa.left + m);
    let y = p.y.min(wa.bottom - h - m).max(wa.top + m);
    if (x, y) != (p.x, p.y) {
        panel.window().set_position(PhysicalPosition::new(x, y));
    }
}
